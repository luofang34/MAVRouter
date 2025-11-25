use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mavlink::common::MavMessage;
use mavlink::MavHeader;
use mavlink::MavlinkVersion;
use mavrouter_rs::router::{EndpointId, RoutedMessage};
use std::sync::Arc;

fn bench_routed_message_clone(c: &mut Criterion) {
    let header = MavHeader::default();
    let message = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let routed = RoutedMessage {
        source_id: EndpointId(0),
        header,
        message: Arc::new(message),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Arc::new(Vec::new()),
    };

    c.bench_function("routed_message_clone", |b| {
        b.iter(|| {
            let _ = black_box(routed.clone());
        })
    });
}

criterion_group!(benches, bench_routed_message_clone);
criterion_main!(benches);
