use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mavlink::MavHeader;
use mavlink::MavlinkVersion;
use mavrouter_rs::mavlink_utils::MessageTarget;
use mavrouter_rs::router::{EndpointId, RoutedMessage};

fn bench_routed_message_clone(c: &mut Criterion) {
    let header = MavHeader::default();
    let routed = RoutedMessage {
        source_id: EndpointId(0),
        header,
        message_id: 0, // HEARTBEAT message ID
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::new(),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    c.bench_function("routed_message_clone", |b| {
        b.iter(|| {
            let _ = black_box(routed.clone());
        })
    });
}

criterion_group!(benches, bench_routed_message_clone);
criterion_main!(benches);
