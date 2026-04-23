//! CPU-scaled stress / churn tests for the routing table.

//! - capacity limits and TTL pruning
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::print_stdout)]

use crate::router::EndpointId;
use crate::routing::*;
use std::sync::Arc;
use std::time::Instant;
// ============================================================================
// Stress (scaled by CPU count via env var)
// ============================================================================

fn stress_iterations() -> usize {
    if let Ok(s) = std::env::var("CI_STRESS_ITERATIONS") {
        return s.parse().expect("CI_STRESS_ITERATIONS must be a number");
    }

    let cpus = num_cpus::get();
    if cpus >= 64 {
        10_000_000
    } else if cpus >= 8 {
        1_000_000
    } else if cpus >= 4 {
        500_000
    } else {
        100_000
    }
}

#[test]
fn test_routing_table_stress_functional() {
    let rt = RoutingTable::new();

    for endpoint in 0..10 {
        for sys in 1..=100 {
            for comp in 1..=10 {
                rt.update(EndpointId(endpoint), sys, comp, Instant::now());
            }
        }
    }

    let iterations = stress_iterations();
    for i in 0..iterations {
        let sys = ((i % 100) + 1) as u8;
        let comp = ((i % 10) + 1) as u8;
        let endpoint = i % 10;
        assert!(
            rt.should_send(EndpointId(endpoint), sys, comp),
            "Route should exist for endpoint {} sys {} comp {}",
            endpoint,
            sys,
            comp
        );
    }
}

#[tokio::test]
async fn test_routing_table_concurrent_access_async() {
    let rt = Arc::new(RoutingTable::new());

    let mut handles = vec![];

    for i in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                rt_clone.update(EndpointId(i), (j % 255) as u8, 1, Instant::now());
            }
        }));
    }

    for _ in 0..10 {
        let rt_clone = rt.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..100 {
                let _ = rt_clone.should_send(EndpointId(0), (j % 255) as u8, 1);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

/// Guardrail: hot-path `should_send` must make forward progress while the
/// group-config is being hammered with writes from a separate task. Before
/// this regression guard landed, `should_send` held a `RwLock<GroupConfig>`
/// read guard across a shard read — a concurrent `set_sniffer_sysids`
/// writer could park the tokio worker thread running `should_send` on a
/// contended write lock, which CLAUDE.md forbids on hot paths.
///
/// With the `ArcSwap<GroupConfig>` conversion, config reads are lock-free
/// atomic loads; the writer's `rcu` CoW never blocks a reader. This test
/// pins down that property: it spawns a writer racing many concurrent
/// `should_send` calls and asserts the read side completes a bounded
/// volume of work in well under a wall-clock budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_send_makes_progress_under_config_write_contention() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    let rt = Arc::new(RoutingTable::new());
    // Seed some routes so `should_send` has real shard work to do.
    let now = Instant::now();
    for ep in 0..16u8 {
        for sys in 1..=64u8 {
            rt.update(EndpointId(ep as usize), sys, 1, now);
        }
    }

    let stop = Arc::new(AtomicBool::new(false));

    // Writer task: hammer the group-config with RCU-style writes. If the
    // read path held a RwLock across shard reads we'd observe the reader
    // stall; with ArcSwap this tight loop completes without blocking.
    let writer_rt = rt.clone();
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        let mut n: u8 = 0;
        while !writer_stop.load(Ordering::Relaxed) {
            writer_rt.set_sniffer_sysids(&[n, n.wrapping_add(1)]);
            writer_rt.set_endpoint_group(EndpointId(n as usize & 0xF), format!("g{}", n & 0x7));
            n = n.wrapping_add(1);
            tokio::task::yield_now().await;
        }
    });

    // Reader tasks: run `should_send` calls. Track completed calls to
    // assert the read path made bounded progress in bounded wall clock.
    let reader_count = 4usize;
    let completed = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::with_capacity(reader_count);
    for r in 0..reader_count {
        let reader_rt = rt.clone();
        let reader_completed = completed.clone();
        let reader_stop = stop.clone();
        readers.push(tokio::spawn(async move {
            let mut i: usize = 0;
            while !reader_stop.load(Ordering::Relaxed) {
                let sys = ((i % 64) + 1) as u8;
                let ep = (r + i) & 0xF;
                let _ = reader_rt.should_send(EndpointId(ep), sys, 1);
                reader_completed.fetch_add(1, Ordering::Relaxed);
                i = i.wrapping_add(1);
                // Yield periodically so the tokio scheduler rotates workers.
                if i & 0xFFF == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }));
    }

    // 250 ms is generous — on any machine, lock-free reads complete
    // millions of iterations in that window. A stall would show up as a
    // near-zero `completed` count.
    tokio::time::sleep(Duration::from_millis(250)).await;
    stop.store(true, Ordering::Relaxed);

    writer.await.unwrap();
    for h in readers {
        h.await.unwrap();
    }

    let done = completed.load(Ordering::Relaxed);
    // Threshold is deliberately conservative (1000 per reader across all
    // reader tasks combined). A regression to `RwLock<GroupConfig>` under
    // contention would blow past this; ArcSwap keeps it lock-free.
    assert!(
        done >= 1_000,
        "reader progress was {done}; expected well over 1000 should_send calls in 250ms — hot path is stalling"
    );
}
