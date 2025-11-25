#![allow(clippy::unwrap_used)]

use mavlink::MavHeader;
use mavrouter_rs::config::EndpointMode;
use mavrouter_rs::dedup::Dedup;
use mavrouter_rs::endpoints::{tcp, udp};
use mavrouter_rs::filter::EndpointFilters;
use mavrouter_rs::router::create_bus;
use mavrouter_rs::routing::RoutingTable;
use parking_lot::{Mutex, RwLock};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[serial]
async fn test_udp_echo() {
    // ... (Existing UDP test logic, assuming it was correct) ...
    // I'll copy-paste the working part to ensure file consistency or just overwrite the whole file.
    // Since I am overwriting, I must include test_udp_echo content.

    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    let bus_tx = bus.clone();
    let bus_rx = bus.subscribe();
    let rt = routing_table.clone();
    let dd = dedup.clone();
    let f = filters.clone();
    let t = token.clone();

    tokio::spawn(async move {
        udp::run(
            1,
            "127.0.0.1:14550".to_string(),
            EndpointMode::Server,
            bus_tx,
            bus_rx,
            rt,
            dd,
            f,
            t,
            300,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect("127.0.0.1:14550").await.unwrap();

    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        ..Default::default()
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();

    socket.send(&buf).await.unwrap();

    // Setup TCP listener (Endpoint 2)
    let bus_tx2 = bus.clone();
    let bus_rx2 = bus.subscribe();
    let rt2 = routing_table.clone();
    let dd2 = dedup.clone();
    let f2 = filters.clone();
    let t2 = token.clone();

    tokio::spawn(async move {
        tcp::run(
            2,
            "127.0.0.1:15760".to_string(),
            EndpointMode::Server,
            bus_tx2,
            bus_rx2,
            rt2,
            dd2,
            f2,
            t2,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut tcp_client = TcpStream::connect("127.0.0.1:15760").await.unwrap();

    // Send UDP -> Router -> TCP
    // Retry logic
    let mut tcp_buf = [0u8; 1024];

    // Send again to be sure TCP client is subscribed
    socket.send(&buf).await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), tcp_client.read(&mut tcp_buf)).await;
    assert!(result.is_ok(), "Timeout waiting for UDP->TCP echo");
    let n = result.unwrap().unwrap();
    assert!(n > 0);
    assert_eq!(tcp_buf[0], 0xFD);
}

#[tokio::test]
#[serial]
async fn test_tcp_bidirectional() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    // Start TCP Server 1 (ID 1)
    let bus_tx = bus.clone();
    let bus_rx = bus.subscribe();
    let rt = routing_table.clone();
    let dd = dedup.clone();
    let f = filters.clone();
    let t = token.clone();

    tokio::spawn(async move {
        tcp::run(
            1,
            "127.0.0.1:15761".to_string(),
            EndpointMode::Server,
            bus_tx,
            bus_rx,
            rt,
            dd,
            f,
            t,
        )
        .await
        .unwrap();
    });

    // Start TCP Server 2 (ID 2)
    let bus_tx2 = bus.clone();
    let bus_rx2 = bus.subscribe();
    let rt2 = routing_table.clone();
    let dd2 = dedup.clone();
    let f2 = filters.clone();
    let t2 = token.clone();

    tokio::spawn(async move {
        tcp::run(
            2,
            "127.0.0.1:15762".to_string(),
            EndpointMode::Server,
            bus_tx2,
            bus_rx2,
            rt2,
            dd2,
            f2,
            t2,
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client1 = TcpStream::connect("127.0.0.1:15761").await.unwrap();
    let mut client3 = TcpStream::connect("127.0.0.1:15762").await.unwrap();

    // Wait for connections to stabilize/subscribe
    tokio::time::sleep(Duration::from_millis(200)).await;

    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        ..Default::default()
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();

    // Send/Recv Loop to avoid races
    let mut rx_buf = [0u8; 1024];
    let mut received = false;

    for _ in 0..5 {
        client1.write_all(&buf).await.unwrap();

        // Short timeout read
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(500), client3.read(&mut rx_buf)).await
        {
            if n > 0 {
                assert_eq!(rx_buf[0], 0xFD);
                received = true;
                break;
            }
        }
    }

    assert!(received, "Failed to receive TCP->TCP message");
}
