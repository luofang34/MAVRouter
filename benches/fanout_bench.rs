#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used)]

//! Bus fan-out benchmark.
//!
//! One producer, N subscribers, M messages. The bus carries
//! `Arc<RoutedMessage>`, so each extra subscriber should only add an atomic
//! refcount bump per message — total work should be `O(N·M)` with the
//! per-message cost *per subscriber* roughly flat as N grows, not `O(N²·M)`
//! as it would be if we broadcast full `RoutedMessage` clones.
//!
//! Run: `cargo bench --features _internal --bench fanout_bench`
//!
//! The criterion baseline for this benchmark is the regression guardrail
//! paired with PR #8: if the per-subscriber cost regresses by more than
//! 10 % against the committed baseline, CI fails.

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mavlink::{MavHeader, MavlinkVersion};
use mavrouter::mavlink_utils::MessageTarget;
use mavrouter::router::{create_bus, EndpointId, RoutedMessage};
use std::sync::Arc;
use tokio::runtime::Runtime;

/// Messages sent per run at every N. Keep this small enough that the
/// benchmark completes in reasonable time at N=64, but large enough that
/// per-iteration variance is noise, not signal.
const MESSAGES_PER_RUN: usize = 1_024;

/// Subscriber counts to sweep over. Keep these powers-of-two so the
/// expected O(N·M) linear scan is visually obvious in the criterion report.
const SUBSCRIBER_COUNTS: &[usize] = &[1, 4, 16, 64];

fn make_routed_message() -> Arc<RoutedMessage> {
    Arc::new(RoutedMessage {
        source_id: EndpointId(0),
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 0,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        // MAVLink v2 HEARTBEAT body size is ~17B — representative, not empty.
        serialized_bytes: Bytes::from_static(&[0u8; 17]),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    })
}

fn bench_bus_fanout(c: &mut Criterion) {
    let rt = Runtime::new().expect("build tokio runtime");
    let mut group = c.benchmark_group("bus_fanout");

    // Per-message throughput across subscriber counts — the BenchmarkId
    // parameter is N, so criterion plots the sweep cleanly.
    for &n in SUBSCRIBER_COUNTS {
        group.throughput(Throughput::Elements(MESSAGES_PER_RUN as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &n,
            |b, &subscriber_count| {
                b.iter_custom(|iters| {
                    rt.block_on(async move {
                        // Bus capacity must exceed MESSAGES_PER_RUN so nothing
                        // lags during the measurement; we're measuring fan-out
                        // cost, not lag-recovery cost.
                        let bus = create_bus(MESSAGES_PER_RUN * 2);

                        let mut handles = Vec::with_capacity(subscriber_count);
                        for _ in 0..subscriber_count {
                            let mut rx = bus.subscribe();
                            handles.push(tokio::spawn(async move {
                                let mut got = 0usize;
                                while got < MESSAGES_PER_RUN * iters as usize {
                                    if rx.recv().await.is_err() {
                                        break;
                                    }
                                    got += 1;
                                }
                                got
                            }));
                        }

                        let msg = make_routed_message();
                        let start = std::time::Instant::now();
                        for _ in 0..iters {
                            for _ in 0..MESSAGES_PER_RUN {
                                // Arc::clone is the cost every subscriber
                                // pays — that's the point of the Arc
                                // migration.
                                //
                                // `.ok()` — broadcast::send returns Err when
                                // all receivers are gone, which is expected
                                // while the receiver tasks are still catching
                                // up to close. The bench measures total send
                                // throughput, so every send counts regardless.
                                bus.tx.send(Arc::clone(&msg)).ok();
                            }
                        }
                        let elapsed = start.elapsed();

                        // Drop the sender so subscribers see `Closed` and
                        // exit; otherwise they'd wait forever.
                        drop(bus);
                        for h in handles {
                            // `.ok()` — a subscriber task panicking would be a
                            // real bug, but at teardown we've already recorded
                            // the elapsed time and just want to join. Any
                            // JoinError here doesn't affect the measurement.
                            h.await.ok();
                        }
                        black_box(elapsed)
                    })
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_bus_fanout);
criterion_main!(benches);
