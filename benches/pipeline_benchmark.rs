#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used)]
#![allow(clippy::indexing_slicing)]
// The latency-summary `println!` at the end of each N sweep is this
// bench's second output channel alongside criterion's own. Benches are
// developer-facing; `print_stdout` is the idiomatic surface.
#![allow(clippy::print_stdout)]

//! End-to-end MAVLink router pipeline benchmark.
//!
//! The existing microbenchmarks in this directory each isolate one
//! primitive (single `should_send` lookup, single filter predicate,
//! single `Arc::clone`). The real hot path composes them: **ingress**
//! does `filter.check_incoming` → `dedup.check_and_insert` → build
//! `Arc<RoutedMessage>` → `bus.send`, and **egress** runs per
//! subscriber as `bus_rx.recv` → `filter.check_outgoing` →
//! `routing_table.should_send` → `writer.write_all(serialized_bytes)`
//! to an async writer.
//!
//! This bench wires all of that together against `tokio::io::sink()`
//! so the I/O call is real (syscall overhead, async state machine)
//! without dragging in a real socket.
//!
//! Two signals are reported:
//!
//! 1. **Criterion throughput** — messages/second at subscriber counts
//!    N ∈ {1, 4, 16, 64}. Criterion prints the usual
//!    mean/stdev/outliers breakdown.
//! 2. **Latency distribution** — per-message p50/p99 over a single
//!    sampled pass per N, printed via `println!` below the criterion
//!    output. "Per-message" is measured from the producer's pre-send
//!    instant to each subscriber's post-write instant, so the sample
//!    set is N · M per N.
//!
//! Run: `cargo bench --features _internal --bench pipeline_benchmark`

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mavlink::{MavHeader, MavlinkVersion};
use mavrouter::dedup::ConcurrentDedup;
use mavrouter::filter::EndpointFilters;
use mavrouter::mavlink_utils::MessageTarget;
use mavrouter::router::{create_bus, EndpointId, RoutedMessage};
use mavrouter::routing::RoutingTable;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::runtime::Runtime;

/// Messages per run. Sized so a 64-subscriber run still completes in a
/// few seconds, and per-iteration variance is noise rather than signal.
const MESSAGES_PER_RUN: usize = 1_024;

/// Subscriber fan-out counts to sweep over. Same set as `fanout_bench`
/// so visualisations line up and a delta between the two benches
/// attributes cleanly to the extra pipeline work on top of raw fan-out.
const SUBSCRIBER_COUNTS: &[usize] = &[1, 4, 16, 64];

/// Build a MAVLink v2 HEARTBEAT frame with sequence byte `seq`. Varying
/// the sequence keeps every frame byte-distinct so the dedup cache
/// misses on every insert (realistic ingress rate).
fn build_heartbeat_bytes(seq: u8) -> Vec<u8> {
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        sequence: seq,
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).expect("encode heartbeat");
    buf
}

/// Pre-build M distinct frames with varying sequence so the producer's
/// hot loop is just iteration + send, no CPU spent on encoding.
fn prebuild_messages(count: usize, source_id: EndpointId) -> Vec<Arc<RoutedMessage>> {
    (0..count)
        .map(|i| {
            let seq = (i & 0xFF) as u8;
            let bytes = build_heartbeat_bytes(seq);
            Arc::new(RoutedMessage {
                source_id,
                header: MavHeader {
                    system_id: 1,
                    component_id: 1,
                    sequence: seq,
                },
                // HEARTBEAT is message_id 0.
                message_id: 0,
                version: MavlinkVersion::V2,
                // Filled in at send time for latency measurement.
                timestamp_us: 0,
                serialized_bytes: Bytes::from(bytes),
                target: MessageTarget {
                    system_id: 0,
                    component_id: 0,
                },
            })
        })
        .collect()
}

fn bench_pipeline_throughput(c: &mut Criterion) {
    let rt = Runtime::new().expect("build tokio runtime");
    let mut group = c.benchmark_group("pipeline_throughput");

    for &n in SUBSCRIBER_COUNTS {
        // Throughput counts ingress messages; each produces N egress
        // writes, so the per-N delta reflects the fan-out cost.
        group.throughput(Throughput::Elements(MESSAGES_PER_RUN as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &n,
            |b, &subscriber_count| {
                b.iter_custom(|iters| {
                    rt.block_on(async move {
                        run_pipeline(subscriber_count, MESSAGES_PER_RUN, iters, None).await
                    })
                });
            },
        );
    }

    group.finish();
}

fn bench_pipeline_latency(c: &mut Criterion) {
    let rt = Runtime::new().expect("build tokio runtime");
    let mut group = c.benchmark_group("pipeline_latency");

    for &n in SUBSCRIBER_COUNTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(n),
            &n,
            |b, &subscriber_count| {
                b.iter_custom(|iters| {
                    rt.block_on(async move {
                        // Per-N sample bucket — one-shot, reset every
                        // outer iter_custom call. Capacity = iters · M · N
                        // to avoid reallocation inside the hot loop.
                        let capacity = (iters as usize)
                            .saturating_mul(MESSAGES_PER_RUN)
                            .saturating_mul(subscriber_count);
                        let samples =
                            Arc::new(parking_lot::Mutex::new(Vec::with_capacity(capacity)));
                        let elapsed = run_pipeline(
                            subscriber_count,
                            MESSAGES_PER_RUN,
                            iters,
                            Some(samples.clone()),
                        )
                        .await;

                        let mut taken = std::mem::take(&mut *samples.lock());
                        taken.sort_unstable();
                        if !taken.is_empty() {
                            let p50 = taken[taken.len() / 2];
                            let p99 = taken[(taken.len() * 99) / 100];
                            let p999 = taken[(taken.len() * 999) / 1000];
                            println!(
                                "pipeline_latency N={} samples={} p50={}us p99={}us p99.9={}us",
                                subscriber_count,
                                taken.len(),
                                p50,
                                p99,
                                p999,
                            );
                        }
                        elapsed
                    })
                });
            },
        );
    }

    group.finish();
}

/// Core pipeline run: spawns `subscriber_count` egress tasks, then
/// produces `iters * messages_per_run` messages through the ingress
/// path. Returns wall-clock elapsed time (for criterion).
///
/// When `latency_samples` is `Some`, each subscriber records the
/// per-message (ingress → post-write) duration in microseconds. The
/// caller reads the samples after the run to compute percentiles.
async fn run_pipeline(
    subscriber_count: usize,
    messages_per_run: usize,
    iters: u64,
    latency_samples: Option<Arc<parking_lot::Mutex<Vec<u64>>>>,
) -> Duration {
    let producer_id = EndpointId(10_000);
    let bus = create_bus(messages_per_run.saturating_mul(2));
    let routing_table = Arc::new(RoutingTable::new());
    // HEARTBEAT target is (0, 0) — broadcast — so `should_send` takes
    // the fast path regardless of the routing table state. We still
    // call it so the measurement reflects the real check.

    let dedup = ConcurrentDedup::new(Duration::from_secs(1));
    let filters = EndpointFilters::default();

    let total_expected = (iters as usize)
        .saturating_mul(messages_per_run)
        .saturating_mul(subscriber_count);
    let drained = Arc::new(AtomicU64::new(0));

    // Spawn subscribers — each runs the full egress branch.
    let mut handles = Vec::with_capacity(subscriber_count);
    for i in 0..subscriber_count {
        let mut rx = bus.subscribe();
        let rt_clone = routing_table.clone();
        let filters = filters.clone();
        let ep_id = EndpointId(i);
        let drained = drained.clone();
        let samples = latency_samples.clone();
        handles.push(tokio::spawn(async move {
            let mut writer = tokio::io::sink();
            let mut local_samples: Vec<u64> = samples.as_ref().map_or_else(Vec::new, |_| {
                Vec::with_capacity(messages_per_run * iters as usize)
            });
            // Exits on `Err(_)` (channel closed). Lagged would also be
            // Err here — we size the bus capacity to avoid lag, so if
            // it fires the bench loop has saturated and breaking out
            // is the right move.
            while let Ok(msg) = rx.recv().await {
                // Egress branch: self-skip → filter → routing → write.
                // Matches EndpointCore::check_outgoing + run_stream_loop's
                // write path.
                if msg.source_id == ep_id {
                    continue;
                }
                if !filters.check_outgoing(&msg.header, msg.message_id) {
                    continue;
                }
                if !rt_clone.should_send(ep_id, msg.target.system_id, msg.target.component_id) {
                    continue;
                }
                writer
                    .write_all(&msg.serialized_bytes)
                    .await
                    .expect("sink writer can't fail");

                if samples.is_some() {
                    // `timestamp_us` is the producer's pre-send Instant
                    // expressed as microseconds since `now_us_since_anchor`'s
                    // process-local anchor. The subtraction below is
                    // meaningful because both sides read the same anchor.
                    let now_us = now_us_since_anchor();
                    let latency = now_us.saturating_sub(msg.timestamp_us);
                    local_samples.push(latency);
                }

                drained.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(samples) = samples {
                samples.lock().extend(local_samples);
            }
        }));
    }

    // Pre-build frames once per outer iter_custom call — the cost of
    // encoding isn't what we're measuring.
    let pool: Vec<Arc<RoutedMessage>> = prebuild_messages(messages_per_run, producer_id);

    let start = Instant::now();
    for _ in 0..iters {
        for template in &pool {
            // Ingress branch: filter.check_incoming → dedup check →
            // Arc::new with timestamp → bus.send.
            let _ = filters.check_incoming(&template.header, template.message_id);
            let _ = dedup.check_and_insert(&template.serialized_bytes);
            let mut msg = RoutedMessage {
                source_id: template.source_id,
                header: template.header,
                message_id: template.message_id,
                version: template.version,
                timestamp_us: 0,
                serialized_bytes: template.serialized_bytes.clone(),
                target: template.target,
            };
            if latency_samples.is_some() {
                msg.timestamp_us = now_us_since_anchor();
            }
            // `.ok()` — broadcast send returns Err if every receiver
            // has already dropped; impossible here because we join
            // subscribers below, but the type still needs handling.
            bus.tx.send(Arc::new(msg)).ok();
        }
    }

    // Wait for every egress write to complete before stopping the
    // clock — we want full-pipeline throughput, not send-side only.
    while drained.load(Ordering::Relaxed) < total_expected as u64 {
        tokio::task::yield_now().await;
    }
    let elapsed = start.elapsed();

    drop(bus);
    for h in handles {
        h.await.ok();
    }
    elapsed
}

/// Microseconds since a process-local fixed anchor (the first call).
/// We want latency as a `u64` in `RoutedMessage::timestamp_us` and
/// `Instant` can't be serialised into that field, so we hash it to
/// microseconds against a constant anchor grabbed once.
fn now_us_since_anchor() -> u64 {
    use std::sync::OnceLock;
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    anchor.elapsed().as_micros() as u64
}

criterion_group!(benches, bench_pipeline_throughput, bench_pipeline_latency);
criterion_main!(benches);
