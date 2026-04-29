#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use super::helpers::make_core;
use crate::endpoint_core::run_stream_loop;
use crate::filter::EndpointFilters;
use crate::mavlink_utils::MessageTarget;
use crate::router::{create_bus, EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use bytes::Bytes;
use mavlink::{common::MavMessage, MavHeader, MavlinkVersion, Message};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_stream_loopback() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RoutingTable::new());
    let token = CancellationToken::new();

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::from_millis(0),
        EndpointFilters::default(),
    );

    let bus_rx = bus.subscribe();

    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    tokio::spawn(async move {
        run_stream_loop(read, write, bus_rx, core, token, "MockSerial".to_string())
            .await
            .unwrap();
    });

    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        ..Default::default()
    };
    let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();

    client.write_all(&buf).await.unwrap();

    let mut bus_rx_check = bus.subscribe();
    let received = bus_rx_check.recv().await.unwrap();
    assert_eq!(received.source_id, EndpointId(1));
    assert_eq!(received.header.system_id, 1);

    let header2 = MavHeader {
        system_id: 2,
        component_id: 1,
        ..Default::default()
    };
    let message = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf_out = Vec::new();
    mavlink::write_v2_msg(&mut buf_out, header2, &message).unwrap();

    let msg_out = RoutedMessage {
        source_id: EndpointId(2),
        header: header2,
        message_id: message.message_id(),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(buf_out),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };
    bus.tx.send(Arc::new(msg_out)).unwrap();

    let mut client_rx_buf = [0u8; 1024];
    let n = client.read(&mut client_rx_buf).await.unwrap();
    assert!(n > 0);
    assert_eq!(client_rx_buf[0], 0xFD);
}
