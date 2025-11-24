use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use mavlink::common::MavMessage;
use mavlink::common::*;
use mavrouter_rs::mavlink_utils::extract_target;
use mavrouter_rs::routing::RoutingTable;
use std::time::Duration; // Explicit import

fn benchmark_routing_table_lookup(c: &mut Criterion) {
    let mut rt = RoutingTable::new();

    // Populate with realistic data: 10 systems, 5 components each
    for sys in 1..=10 {
        for comp in 1..=5 {
            rt.update(sys as usize, sys, comp);
        }
    }

    c.bench_function("routing_lookup_hit", |b| {
        b.iter(|| black_box(rt.should_send(1, 5, 3)))
    });

    c.bench_function("routing_lookup_miss", |b| {
        b.iter(|| black_box(rt.should_send(1, 99, 1)))
    });

    c.bench_function("routing_lookup_broadcast", |b| {
        b.iter(|| black_box(rt.should_send(1, 0, 0)))
    });
}

fn benchmark_target_extraction(c: &mut Criterion) {
    let cmd = COMMAND_LONG_DATA {
        target_system: 1,
        target_component: 2,
        command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
        confirmation: 0,
        param1: 0.0,
        param2: 0.0,
        param3: 0.0,
        param4: 0.0,
        param5: 0.0,
        param6: 0.0,
        param7: 0.0,
    };
    let msg = MavMessage::COMMAND_LONG(cmd);

    c.bench_function("extract_target_command", |b| {
        b.iter(|| black_box(extract_target(&msg)))
    });

    let hb = MavMessage::HEARTBEAT(HEARTBEAT_DATA::default());
    c.bench_function("extract_target_broadcast", |b| {
        b.iter(|| black_box(extract_target(&hb)))
    });
}

fn benchmark_routing_table_update(c: &mut Criterion) {
    c.bench_function("routing_update", |b| {
        let mut rt = RoutingTable::new();
        let mut counter = 0u8;
        b.iter(|| {
            counter = counter.wrapping_add(1);
            rt.update(1, counter, 1);
        })
    });
}

fn benchmark_routing_table_prune(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing_prune");

    for size in [10, 100, 1000].iter() {
        let mut rt = RoutingTable::new();
        for i in 0..*size {
            rt.update(1, (i % 255) as u8, 1);
        }

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                rt.prune(Duration::from_secs(300));
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    benchmark_routing_table_lookup,
    benchmark_target_extraction,
    benchmark_routing_table_update,
    benchmark_routing_table_prune
);
criterion_main!(benches);
