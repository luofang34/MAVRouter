//! Stress test for the routing-table updater path.
//!
//! Verifies the concurrency claim that motivated this design: the routing
//! hot path must **not** block a tokio worker thread. The old code took
//! `RwLock::write()` as a fallback when `try_write()` failed, which could
//! starve unrelated tasks under contention. The current design routes all
//! route observations through an mpsc channel consumed by a single
//! [`mavrouter::orchestration::spawn_routing_updater`] task, so writers on
//! the async side only ever do non-blocking `try_send`.
//!
//! This test:
//!   - spawns 20 writer tasks and 20 reader tasks,
//!   - lets them hammer the routing table for 10 seconds,
//!   - asserts every single task made *substantial* progress.
//!
//! If any task's tick counter is still tiny at the end (i.e. it was
//! starved while others ran), the test fails.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mavrouter::orchestration::spawn_routing_updater;
use mavrouter::router::EndpointId;
use mavrouter::routing::{RouteUpdate, RoutingTable};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const WRITER_COUNT: usize = 20;
const READER_COUNT: usize = 20;
const RUN_DURATION: Duration = Duration::from_secs(10);

/// Minimum number of ticks we demand from every single task. With a 10s
/// budget and per-iteration work measured in microseconds (an mpsc
/// `try_send` for writers, a parking_lot read + a couple of HashMap lookups
/// for readers), each task should easily clear many thousands of ticks. We
/// pick 1_000 to stay far above any noise floor but still be robust to a
/// slow CI runner. If any task falls below this, the scheduler was
/// starving it — which is exactly the regression we're guarding against.
const MIN_TICKS_PER_TASK: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routing_writer_and_reader_fairness_under_contention() {
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let (update_tx, update_rx) = mpsc::channel::<RouteUpdate>(4096);
    let cancel = CancellationToken::new();

    // Single updater task owns all writes — exactly the production topology.
    let updater = spawn_routing_updater(
        routing_table.clone(),
        update_rx,
        Duration::from_secs(300),
        Duration::from_secs(60),
        cancel.child_token(),
    );

    // Per-task progress counters; final assertions read these.
    let writer_ticks: Vec<Arc<AtomicU64>> = (0..WRITER_COUNT)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let reader_ticks: Vec<Arc<AtomicU64>> = (0..READER_COUNT)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();

    let mut handles = Vec::new();

    // Writers: each claims its own EndpointId and cycles through sys_ids,
    // submitting updates via the same mpsc the production hot path uses.
    // `try_send` — *never* blocks, which is the invariant under test.
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
                // We don't care whether the channel is momentarily full;
                // the test is asserting *our* task kept making progress,
                // not that every update landed.
                tx.try_send(update).ok();
                ticks.fetch_add(1, Ordering::Relaxed);
                sys_id = sys_id.wrapping_add(1);
                if sys_id == 0 {
                    sys_id = 1;
                }
                // Yield so the tokio runtime can schedule other tasks —
                // without this, a single-threaded runtime could let one
                // writer hog the worker and make the starvation check
                // false-pass trivially.
                tokio::task::yield_now().await;
            }
        }));
    }

    // Readers: take a read lock, make a routing decision, release. This is
    // the same access pattern as `check_outgoing` on the hot path.
    for (i, ticks) in reader_ticks.iter().enumerate() {
        let rt = routing_table.clone();
        let ticks = ticks.clone();
        let cancel = cancel.clone();
        let endpoint_id = EndpointId(2000 + i);
        handles.push(tokio::spawn(async move {
            let mut sys_id: u8 = 1;
            while !cancel.is_cancelled() {
                let decision = {
                    let guard = rt.read();
                    guard.should_send(endpoint_id, sys_id, 1)
                };
                // Touch the decision so the optimiser can't delete the read.
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

    // Reap everyone. We allow a small grace window for tasks to observe
    // cancellation and return — they don't hold any resources that matter.
    for h in handles {
        tokio::time::timeout(Duration::from_secs(2), h).await.ok();
    }
    // Close the update channel so the updater can exit too.
    drop(update_tx);
    tokio::time::timeout(Duration::from_secs(2), updater.handle)
        .await
        .ok();

    // Starvation check: every single task must have cleared MIN_TICKS.
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

    // Sanity check: aggregate throughput should be *much* higher than the
    // per-task floor. If we only cleared the floor, something is wrong
    // even though no individual task is "starving".
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

    // And routing-table state should actually have changed — otherwise the
    // mpsc→updater path wasn't doing anything.
    let stats = routing_table.read().stats();
    assert!(
        stats.total_systems > 0,
        "updater task applied no updates — check channel wiring"
    );
}
