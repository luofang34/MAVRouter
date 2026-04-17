#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used)]

//! Contention bench: `RoutingTable::should_send` throughput when a single
//! writer is hammering the table in the background.
//!
//! The internal sharding claim is that read traffic for `sys_id=X`
//! doesn't stall behind a write for `sys_id=Y` when they hash to
//! different shards. The bench spins up a background writer that cycles
//! through sysids while the benchmark measures `should_send` rate. With
//! a single global `RwLock` the reader would block every time the writer
//! takes a write lock; with per-shard locks the reader only blocks when
//! the writer happens to hit the same shard.
//!
//! Run: `cargo bench --features _internal --bench routing_contention_bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mavrouter::router::EndpointId;
use mavrouter::routing::RoutingTable;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Reader benchmark: measure `should_send` rate while a background thread
/// writes continuously. We don't assert on the numbers here — criterion
/// records them and the comparison-to-main run in CI (or the
/// self-hosted bench runner) picks up regressions.
fn bench_reader_under_writer(c: &mut Criterion) {
    let rt = Arc::new(RoutingTable::new());

    // Pre-populate so should_send has something to do.
    for sys in 1..=64u8 {
        for comp in 1..=4u8 {
            rt.update(EndpointId(1), sys, comp, Instant::now());
        }
    }

    let stop = Arc::new(AtomicBool::new(false));

    let writer = {
        let rt = rt.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut sys: u8 = 1;
            while !stop.load(Ordering::Relaxed) {
                rt.update(EndpointId(2), sys, 1, Instant::now());
                sys = sys.wrapping_add(1);
                if sys == 0 {
                    sys = 1;
                }
            }
        })
    };

    // Give the writer time to actually start before we measure.
    thread::sleep(Duration::from_millis(50));

    c.bench_function("should_send_with_background_writer", |b| {
        let mut target_sys: u8 = 1;
        b.iter(|| {
            target_sys = target_sys.wrapping_add(1);
            if target_sys == 0 {
                target_sys = 1;
            }
            // Use a different endpoint id from the writer's so the write
            // and read don't thrash the same entry — the bench is about
            // lock contention, not cache contention.
            black_box(rt.should_send(EndpointId(1), target_sys, 1))
        })
    });

    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer thread panicked");
}

criterion_group!(benches, bench_reader_under_writer);
criterion_main!(benches);
