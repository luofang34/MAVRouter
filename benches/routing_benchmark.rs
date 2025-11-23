//! Performance benchmarks for MAVLink Router
//!
//! Run with: cargo bench
//!
//! This file provides benchmarks for core routing components.
//! To use these benchmarks, add the following to Cargo.toml:
//!
//! [dev-dependencies]
//! criterion = "0.5"
//!
//! [[bench]]
//! name = "routing_benchmark"
//! harness = false

#[cfg(feature = "benchmarks")]
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

#[cfg(feature = "benchmarks")]
fn benchmark_message_parsing(c: &mut Criterion) {
    // Example MAVLink HEARTBEAT message (v2.0)
    let heartbeat = vec![
        0xFD, // MAVLink 2.0 magic
        0x09, // Payload length
        0x00, // Incompatibility flags
        0x00, // Compatibility flags
        0x00, // Sequence
        0x01, // System ID
        0x01, // Component ID
        0x00, 0x00, 0x00, // Message ID (HEARTBEAT = 0)
        // Payload (9 bytes)
        0x00, 0x00, 0x00, 0x00, // custom_mode
        0x06, // type (MAV_TYPE_GCS)
        0x08, // autopilot (MAV_AUTOPILOT_INVALID)
        0x00, // base_mode
        0x04, // system_status (MAV_STATE_ACTIVE)
        0x03, // mavlink_version
        // CRC (2 bytes)
        0x00, 0x00,
    ];

    let mut group = c.benchmark_group("message_parsing");
    group.throughput(Throughput::Bytes(heartbeat.len() as u64));

    group.bench_function("parse_heartbeat", |b| {
        b.iter(|| {
            // Simulate parsing
            black_box(&heartbeat)
        });
    });

    group.finish();
}

#[cfg(feature = "benchmarks")]
fn benchmark_routing_table(c: &mut Criterion) {
    use std::time::Duration;

    let mut group = c.benchmark_group("routing_table");

    group.bench_function("insert_route", |b| {
        b.iter(|| {
            // Simulate route insertion
            black_box((1u8, 1u8, 0usize));
        });
    });

    group.bench_function("lookup_route", |b| {
        b.iter(|| {
            // Simulate route lookup
            black_box((1u8, 1u8));
        });
    });

    group.finish();
}

#[cfg(feature = "benchmarks")]
fn benchmark_message_filtering(c: &mut Criterion) {
    let mut group = c.benchmark_group("filtering");

    let allowed_ids = vec![0, 1, 30, 33]; // Common message IDs

    group.bench_function("filter_check_allow", |b| {
        b.iter(|| {
            let msg_id = black_box(0u32);
            allowed_ids.contains(&msg_id)
        });
    });

    group.bench_function("filter_check_block", |b| {
        b.iter(|| {
            let msg_id = black_box(999u32);
            !allowed_ids.contains(&msg_id)
        });
    });

    group.finish();
}

#[cfg(feature = "benchmarks")]
criterion_group!(
    benches,
    benchmark_message_parsing,
    benchmark_routing_table,
    benchmark_message_filtering
);

#[cfg(feature = "benchmarks")]
criterion_main!(benches);

#[cfg(not(feature = "benchmarks"))]
fn main() {
    println!("Benchmarks are disabled. Enable with:");
    println!("  cargo bench --features benchmarks");
    println!("\nFirst, add to Cargo.toml:");
    println!("  [features]");
    println!("  benchmarks = []");
    println!("\n  [dev-dependencies]");
    println!("  criterion = \"0.5\"");
    println!("\n  [[bench]]");
    println!("  name = \"routing_benchmark\"");
    println!("  harness = false");
}
