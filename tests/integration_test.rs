use mavrouter_rs::config::EndpointMode;
use mavrouter_rs::router::create_bus;
use mavrouter_rs::routing::RoutingTable;
use mavrouter_rs::dedup::Dedup;
use mavrouter_rs::filter::EndpointFilters;
use mavrouter_rs::endpoints::{udp, tcp};
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use std::time::Duration;
use tokio::net::{UdpSocket, TcpStream};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use mavlink::MavHeader;
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_udp_echo() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    // Start UDP Endpoint 1 (Server)
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
            t
        ).await.unwrap();
    });

    // Wait for bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test Client
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect("127.0.0.1:14550").await.unwrap();

    // Send Heartbeat (Sys 1)
    let header = MavHeader { system_id: 1, component_id: 1, ..Default::default() };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();
    
    socket.send(&buf).await.unwrap();

    // We expect NO echo because source_id check (if we were endpoint 1).
    // But we are external client.
    // Router receives.
    // Does it broadcast back to UDP?
    // UDP endpoint: "If msg.source_id == id { continue; }"
    // Message came from UDP (ID 1). So it is NOT sent back to UDP.
    // Correct.
    
    // To verify receiving, we need ANOTHER endpoint (ID 2).
    // Let's add a TCP endpoint.
    
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
            t2
        ).await.unwrap();
    });
    
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let mut tcp_client = TcpStream::connect("127.0.0.1:15760").await.unwrap();
    
    // Send UDP -> Router -> TCP
    socket.send(&buf).await.unwrap();
    
    let mut tcp_buf = [0u8; 1024];
    let n = tcp_client.read(&mut tcp_buf).await.unwrap();
    assert!(n > 0);
    // Basic check
    assert_eq!(tcp_buf[0], 0xFD); // Magic V2
}
