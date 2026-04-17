//! Unit tests for the orchestration-layer primitives: the shutdown
//! helper's behaviour when tasks are cooperative vs. stuck, and the
//! routing updater's ability to keep async tasks unstarved under
//! contention.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]

use super::*;
use crate::router::EndpointId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_test::traced_test;

// ============================================================================
// shutdown_with_timeout
// ============================================================================

fn spawn_stuck_task(name: &str) -> NamedTask {
    let handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    NamedTask::new(name, handle)
}

#[tokio::test(flavor = "current_thread")]
#[traced_test]
async fn shutdown_with_timeout_reports_and_aborts_stuck_tasks() {
    let stuck = spawn_stuck_task("Stuck Task A");
    let abort_handle = stuck.handle.abort_handle();

    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(vec![stuck], Duration::from_millis(500)).await;
    let elapsed = start.elapsed();

    assert!(
        !clean_exit,
        "timeout path must report !clean_exit, got true after {:?}",
        elapsed
    );
    // Public contract is "returns within 6 seconds"; the 500ms budget used
    // here is strictly tighter so the assertion still binds.
    assert!(
        elapsed < Duration::from_secs(6),
        "shutdown_with_timeout must return within 6s even for stuck tasks, took {:?}",
        elapsed
    );
    assert!(
        logs_contain("Shutdown timed out"),
        "expected an error log containing 'Shutdown timed out'"
    );
    assert!(
        logs_contain("Stuck Task A"),
        "expected the stuck task name to be enumerated in the timeout log"
    );

    for _ in 0..20 {
        if abort_handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        abort_handle.is_finished(),
        "stuck task should have been aborted after the timeout"
    );
}

#[tokio::test(flavor = "current_thread")]
#[traced_test]
async fn shutdown_with_timeout_is_fast_path_for_cooperative_tasks() {
    let token = CancellationToken::new();
    let cooperative_token = token.clone();
    let cooperative = NamedTask::new(
        "Cooperative Task",
        tokio::spawn(async move {
            cooperative_token.cancelled().await;
        }),
    );

    token.cancel();

    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(vec![cooperative], Duration::from_secs(5)).await;
    let elapsed = start.elapsed();

    assert!(clean_exit, "cooperative task must hit the Ok branch");
    assert!(
        elapsed < Duration::from_secs(1),
        "cooperative shutdown should finish well inside the budget, took {:?}",
        elapsed
    );
    assert!(
        !logs_contain("Shutdown timed out"),
        "no timeout log should be emitted when all tasks exit cleanly"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_with_timeout_is_noop_for_empty_task_list() {
    let start = Instant::now();
    let clean_exit = shutdown_with_timeout(Vec::new(), Duration::from_millis(1)).await;
    assert!(clean_exit);
    assert!(start.elapsed() < Duration::from_millis(50));
}

// ============================================================================
// spawn_routing_updater contention / fairness
// ============================================================================

const WRITER_COUNT: usize = 20;
const READER_COUNT: usize = 20;
const RUN_DURATION: Duration = Duration::from_secs(10);

// Per-task floor — with a 10s budget and work measured in microseconds per
// iteration, each task should clear many thousands of ticks. 1_000 is above
// any scheduler noise and a long way from the aggregate throughput the
// whole fleet actually hits; any task below that was being starved.
const MIN_TICKS_PER_TASK: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routing_writer_and_reader_fairness_under_contention() {
    let routing_table = Arc::new(RoutingTable::new());
    let (update_tx, update_rx) = mpsc::channel::<RouteUpdate>(4096);
    let cancel = CancellationToken::new();

    let updater = spawn_routing_updater(
        routing_table.clone(),
        update_rx,
        Duration::from_secs(300),
        Duration::from_secs(60),
        cancel.child_token(),
    );

    let writer_ticks: Vec<Arc<AtomicU64>> = (0..WRITER_COUNT)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let reader_ticks: Vec<Arc<AtomicU64>> = (0..READER_COUNT)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let mut handles = Vec::new();

    for (i, ticks) in writer_ticks.iter().enumerate() {
        let tx = update_tx.clone();
        let ticks = ticks.clone();
        let cancel = cancel.clone();
        let endpoint_id = EndpointId(1000 + i);
        handles.push(tokio::spawn(async move {
            let mut sys_id: u8 = 1;
            while !cancel.is_cancelled() {
                let update = RouteUpdate {
                    endpoint_id,
                    sys_id,
                    comp_id: 1,
                    now: Instant::now(),
                };
                // try_send never blocks — full queue = drop + retry next tick.
                tx.try_send(update).ok();
                ticks.fetch_add(1, Ordering::Relaxed);
                sys_id = sys_id.wrapping_add(1);
                if sys_id == 0 {
                    sys_id = 1;
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    for (i, ticks) in reader_ticks.iter().enumerate() {
        let rt = routing_table.clone();
        let ticks = ticks.clone();
        let cancel = cancel.clone();
        let endpoint_id = EndpointId(2000 + i);
        handles.push(tokio::spawn(async move {
            let mut sys_id: u8 = 1;
            while !cancel.is_cancelled() {
                let decision = rt.should_send(endpoint_id, sys_id, 1);
                std::hint::black_box(decision);
                ticks.fetch_add(1, Ordering::Relaxed);
                sys_id = sys_id.wrapping_add(1);
                if sys_id == 0 {
                    sys_id = 1;
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    tokio::time::sleep(RUN_DURATION).await;
    cancel.cancel();

    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h).await.ok();
    }
    drop(update_tx);
    tokio::time::timeout(Duration::from_secs(2), updater.handle)
        .await
        .ok();

    for (i, ticks) in writer_ticks.iter().enumerate() {
        let v = ticks.load(Ordering::Relaxed);
        assert!(
            v >= MIN_TICKS_PER_TASK,
            "writer {} only ticked {} times in {:?} — starvation?",
            i,
            v,
            RUN_DURATION
        );
    }
    for (i, ticks) in reader_ticks.iter().enumerate() {
        let v = ticks.load(Ordering::Relaxed);
        assert!(
            v >= MIN_TICKS_PER_TASK,
            "reader {} only ticked {} times in {:?} — starvation?",
            i,
            v,
            RUN_DURATION
        );
    }

    let writer_total: u64 = writer_ticks.iter().map(|a| a.load(Ordering::Relaxed)).sum();
    let reader_total: u64 = reader_ticks.iter().map(|a| a.load(Ordering::Relaxed)).sum();
    assert!(
        writer_total > 100_000,
        "aggregate writer throughput surprisingly low: {}",
        writer_total
    );
    assert!(
        reader_total > 100_000,
        "aggregate reader throughput surprisingly low: {}",
        reader_total
    );

    let stats = routing_table.as_ref().stats();
    assert!(
        stats.total_systems > 0,
        "updater task applied no updates — check channel wiring"
    );
}
