//! Intelligent routing behaviour through the public [`Router`] API.
//!
//! These tests stand up a `Router` with multiple TCP-server endpoints,
//! have each client "announce" itself by sending a HEARTBEAT (which the
//! router learns into its routing table), then send a targeted command
//! and assert only the intended recipient sees it.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing)]

use mavlink::{MavHeader, Message};
use mavrouter::Router;
use serial_test::serial;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn read_mavlink(client: &mut TcpStream) -> Option<(MavHeader, mavlink::common::MavMessage)> {
    let mut buf = [0u8; 1024];
    match tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            let mut cursor = std::io::Cursor::new(&buf[..n]);
            mavlink::read_v2_msg(&mut cursor)
                .or_else(|_| {
                    cursor.set_position(0);
                    mavlink::read_v1_msg(&mut cursor)
                })
                .ok()
        }
        _ => None,
    }
}

fn heartbeat_bytes(system_id: u8) -> Vec<u8> {
    let header = MavHeader {
        system_id,
        component_id: 1,
        sequence: 0,
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();
    buf
}

fn command_long_bytes(target_sys: u8, source_sys: u8) -> Vec<u8> {
    let cmd = mavlink::common::COMMAND_LONG_DATA {
        target_system: target_sys,
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
    let header = MavHeader {
        system_id: source_sys,
        component_id: 1,
        sequence: 1,
    };
    let mut buf = Vec::new();
    mavlink::write_v2_msg(
        &mut buf,
        header,
        &mavlink::common::MavMessage::COMMAND_LONG(cmd),
    )
    .unwrap();
    buf
}

#[tokio::test]
#[serial]
async fn test_targeted_message_routing() {
    let toml = r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:16001"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:16002"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:16003"
mode = "server"
"#;

    let router = Router::from_str(toml).await.expect("router should start");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client1 = TcpStream::connect("127.0.0.1:16001").await.unwrap();
    let mut client2 = TcpStream::connect("127.0.0.1:16002").await.unwrap();
    let mut client3 = TcpStream::connect("127.0.0.1:16003").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Each client announces a unique system id so the router learns where
    // each MAVLink system lives.
    client1.write_all(&heartbeat_bytes(1)).await.unwrap();
    client2.write_all(&heartbeat_bytes(2)).await.unwrap();
    client3.write_all(&heartbeat_bytes(3)).await.unwrap();

    // Let the routing updater absorb the heartbeats and drain any broadcast
    // echoes that landed on the other sockets.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while client1.try_read(&mut [0u8; 1024]).is_ok() {}
    while client2.try_read(&mut [0u8; 1024]).is_ok() {}
    while client3.try_read(&mut [0u8; 1024]).is_ok() {}

    // Targeted COMMAND_LONG from client1 to system 2.
    client1.write_all(&command_long_bytes(2, 1)).await.unwrap();

    let (header, msg) = read_mavlink(&mut client2)
        .await
        .expect("client2 should receive the targeted command");
    assert_eq!(header.system_id, 1, "source is system 1");
    assert_eq!(msg.message_id(), 76, "COMMAND_LONG message id");

    // client3 must NOT receive it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Ok(n) = client3.try_read(&mut [0u8; 1024]) {
        assert_eq!(
            n, 0,
            "client3 should not see a command targeted at system 2"
        );
    }

    // Sender must not receive its own message echoed.
    if let Ok(n) = client1.try_read(&mut [0u8; 1024]) {
        assert_eq!(n, 0, "client1 should not see its own command");
    }

    router.stop().await;
}

#[tokio::test]
#[serial]
async fn test_unknown_target_dropped() {
    let toml = r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:16010"
mode = "server"
"#;

    let router = Router::from_str(toml).await.expect("router should start");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = TcpStream::connect("127.0.0.1:16010").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Client announces as system 1.
    client.write_all(&heartbeat_bytes(1)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Drain own heartbeat echo (if any).
    while client.try_read(&mut [0u8; 1024]).is_ok() {}

    // Send a command targeted at an unknown system (200). Because we're
    // the only endpoint and we don't claim to have seen system 200, the
    // router should drop it.
    client.write_all(&command_long_bytes(200, 1)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    if let Ok(n) = client.try_read(&mut [0u8; 1024]) {
        assert_eq!(n, 0, "message to unknown system should be dropped");
    }

    router.stop().await;
}
