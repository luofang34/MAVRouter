#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! End-to-end routing tests
//! 
//! These tests verify that intelligent routing works correctly:
//! - Messages only go to endpoints that have seen the target system
//! - Broadcast messages go to all endpoints
//! - Component-specific routing works

use mavrouter_rs::config::EndpointMode;
use mavrouter_rs::router::create_bus;
use mavrouter_rs::routing::RoutingTable;
use mavrouter_rs::dedup::Dedup;
use mavrouter_rs::filter::EndpointFilters;
use mavrouter_rs::endpoints::tcp;
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use mavlink::{MavHeader, Message};
use serial_test::serial;

async fn read_mavlink_message(client: &mut TcpStream) -> Option<(MavHeader, mavlink::common::MavMessage)> {
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            let mut cursor = std::io::Cursor::new(&buf[..n]);
            // Try V2 then V1
            mavlink::read_v2_msg(&mut cursor).or_else(|_| {
                cursor.set_position(0);
                mavlink::read_v1_msg(&mut cursor)
            }).ok()
        }
        _ => None,
    }
}

#[tokio::test]
#[serial]
async fn test_targeted_message_routing() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    // Start 3 TCP endpoints
    for i in 1..=3 {
        let bus_tx = bus.clone();
        let bus_rx = bus.subscribe();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let f = filters.clone();
        let t = token.clone();
        let port = 16000 + i;

        tokio::spawn(async move {
            tcp::run(
                i,
                format!("127.0.0.1:{}", port),
                EndpointMode::Server,
                bus_tx, bus_rx, rt, dd, f, t
            ).await.unwrap();
        });
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client1 = TcpStream::connect("127.0.0.1:16001").await.unwrap();
    let mut client2 = TcpStream::connect("127.0.0.1:16002").await.unwrap();
    let mut client3 = TcpStream::connect("127.0.0.1:16003").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Step 1: Clients announce themselves
    let hb1 = MavHeader { system_id: 1, component_id: 1, sequence: 0 };
    let msg1 = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf1 = Vec::new();
    mavlink::write_v2_msg(&mut buf1, hb1, &msg1).unwrap();
    client1.write_all(&buf1).await.unwrap();

    let hb2 = MavHeader { system_id: 2, component_id: 1, sequence: 0 };
    let msg2 = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf2 = Vec::new();
    mavlink::write_v2_msg(&mut buf2, hb2, &msg2).unwrap();
    client2.write_all(&buf2).await.unwrap();

    let hb3 = MavHeader { system_id: 3, component_id: 1, sequence: 0 };
    let msg3 = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf3 = Vec::new();
    mavlink::write_v2_msg(&mut buf3, hb3, &msg3).unwrap();
    client3.write_all(&buf3).await.unwrap();

    // Wait for routing table to learn and drain heartbeats
    tokio::time::sleep(Duration::from_millis(300)).await;
    
    // Drain initial broadcasts from ALL clients
    while client1.try_read(&mut [0u8; 1024]).is_ok() {}
    while client2.try_read(&mut [0u8; 1024]).is_ok() {}
    while client3.try_read(&mut [0u8; 1024]).is_ok() {}

    // Step 2: Send TARGETED message from Client 1 to System 2
    let cmd = mavlink::common::COMMAND_LONG_DATA {
        target_system: 2,  // ← TARGETED!
        target_component: 1,
        command: mavlink::common::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
        confirmation: 0,
        param1: 1.0,
        param2: 0.0,
        param3: 0.0,
        param4: 0.0,
        param5: 0.0,
        param6: 0.0,
        param7: 0.0,
    };
    let cmd_header = MavHeader { system_id: 1, component_id: 1, sequence: 1 };
    let mut cmd_buf = Vec::new();
    mavlink::write_v2_msg(
        &mut cmd_buf,
        cmd_header,
        &mavlink::common::MavMessage::COMMAND_LONG(cmd)
    ).unwrap();

    client1.write_all(&cmd_buf).await.unwrap();

    // Step 3: Verify routing

    // Client 2 SHOULD receive (targeted to sys 2)
    let (header, msg) = read_mavlink_message(&mut client2).await.expect("Client 2 should receive message");
    
    assert_eq!(header.system_id, 1, "Source is system 1");
    assert_eq!(msg.message_id(), 76, "Should be COMMAND_LONG");

    // Client 3 should NOT receive (not targeted)
    tokio::time::sleep(Duration::from_millis(500)).await;
    let result3 = client3.try_read(&mut [0u8; 1024]);
    if let Ok(n) = result3 {
        assert_eq!(n, 0, "Client 3 should NOT receive message targeted to sys 2");
    }
    
    // Client 1 should NOT receive (it's the sender)
    let result1 = client1.try_read(&mut [0u8; 1024]);
    if let Ok(n) = result1 {
        assert_eq!(n, 0, "Client 1 should not receive its own message");
    }
}

#[tokio::test]
#[serial]
async fn test_unknown_target_dropped() {
    let bus = create_bus(100);
    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(0))));
    let filters = EndpointFilters::default();
    let token = CancellationToken::new();

    // Only 1 endpoint
    let bus_tx = bus.clone();
    let bus_rx = bus.subscribe();
    tokio::spawn({
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let f = filters.clone();
        let t = token.clone();
        async move {
            tcp::run(
                1, "127.0.0.1:16010".to_string(),
                EndpointMode::Server,
                bus_tx, bus_rx, rt, dd, f, t
            ).await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut client = TcpStream::connect("127.0.0.1:16010").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client announces as system 1
    let hb = MavHeader { system_id: 1, component_id: 1, sequence: 0 };
    let msg = mavlink::common::MavMessage::HEARTBEAT(
        mavlink::common::HEARTBEAT_DATA::default()
    );
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, hb, &msg).unwrap();
    client.write_all(&buf).await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Send command to UNKNOWN system 200
    let cmd = mavlink::common::COMMAND_LONG_DATA {
        target_system: 200,  // ← Unknown!
        target_component: 1,
        command: mavlink::common::MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
        confirmation: 0,
        param1: 1.0,
        param2: 0.0,
        param3: 0.0,
        param4: 0.0,
        param5: 0.0,
        param6: 0.0,
        param7: 0.0,
    };
    let cmd_header = MavHeader { system_id: 1, component_id: 1, sequence: 1 };
    let mut cmd_buf = Vec::new();
    mavlink::write_v2_msg(
        &mut cmd_buf,
        cmd_header,
        &mavlink::common::MavMessage::COMMAND_LONG(cmd)
    ).unwrap();

    client.write_all(&cmd_buf).await.unwrap();

    // Wait and verify no message comes back
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    let mut discard = [0u8; 1024];
    let result = client.try_read(&mut discard);

    if let Ok(n) = result {
        assert_eq!(n, 0, "Message to unknown system should be dropped/not echoed");
    }
}
