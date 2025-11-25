#![allow(clippy::unwrap_used)]

use bytes::Bytes;
use mavlink::{MavHeader, MavlinkVersion};
use mavrouter_rs::dedup::Dedup;
use mavrouter_rs::endpoint_core::{run_stream_loop, EndpointCore};
use mavrouter_rs::filter::EndpointFilters;
use mavrouter_rs::router::{create_bus, EndpointId, RoutedMessage};
use mavrouter_rs::routing::RoutingTable;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_stream_loopback() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    let bus_tx = bus.clone();
    let bus_rx = bus.subscribe();

    let core = EndpointCore {
        id: EndpointId(1),
        bus_tx,
        routing_table,
        dedup,
        filters,
    };

    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    tokio::spawn(async move {
        run_stream_loop(read, write, bus_rx, core, token, "MockSerial".to_string())
            .await
            .unwrap();
    });

    // Test: Write to Client (Input to Endpoint) -> Check Bus
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        ..Default::default()
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();

    client.write_all(&buf).await.unwrap();

    let mut bus_rx_check = bus.subscribe();
    let received = bus_rx_check.recv().await.unwrap();
    assert_eq!(received.source_id, EndpointId(1));
    assert_eq!(received.header.system_id, 1);

    // Test: Send to Bus (Output from Endpoint) -> Read from Client
    let header = MavHeader {
        system_id: 2,
        component_id: 1,
        ..Default::default()
    };
    let message =
        mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf_out = Vec::new();
    mavlink::write_v2_msg(&mut buf_out, header, &message).unwrap();

    let msg_out = RoutedMessage {
        source_id: EndpointId(2), // From another endpoint
        header,
        message: Arc::new(message),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(buf_out),
    };
    bus.send(msg_out).unwrap();

    let mut client_rx_buf = [0u8; 1024];
    let n = client.read(&mut client_rx_buf).await.unwrap();
    assert!(n > 0);
    assert_eq!(client_rx_buf[0], 0xFD); // Magic V2
}
