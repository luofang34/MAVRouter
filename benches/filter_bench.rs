use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mavrouter_rs::filter::EndpointFilters;
use mavlink::MavHeader;

fn bench_filter_hashset_100_entries(c: &mut Criterion) {
    let mut filters = EndpointFilters::default();
    // Populate with 100 allowed message IDs
    for i in 0..100 {
        filters.allow_msg_id_out.insert(i);
    }

    let header = MavHeader::default();

    c.bench_function("filter_check_outgoing_hashset_hit", |b| {
        b.iter(|| {
            // Check ID 50 (Hit)
            filters.check_outgoing(black_box(&header), black_box(50))
        })
    });

    c.bench_function("filter_check_outgoing_hashset_miss", |b| {
        b.iter(|| {
            // Check ID 150 (Miss)
            filters.check_outgoing(black_box(&header), black_box(150))
        })
    });
}

criterion_group!(benches, bench_filter_hashset_100_entries);
criterion_main!(benches);
