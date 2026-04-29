//! Intelligent routing behaviour through the public [`Router`] API.
//!
//! These tests stand up a `Router` with multiple TCP-server endpoints,
//! have each client "announce" itself by sending a HEARTBEAT (which the
//! router learns into its routing table), then send a targeted command
//! and assert only the intended recipient sees it.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::panic)]
#![allow(clippy::arithmetic_side_effects)]

use mavlink::{MavHeader, Message};
use mavrouter::{MessageBus, Router, RoutingTable};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Wait until the router's bus has at least `want` subscribers.
/// Replaces blind `tokio::time::sleep` per CLAUDE.md "synchronize with
/// events, not sleep" — `receiver_count()` is the concrete observable.
async fn wait_for_bus_subscribers(bus: &MessageBus, want: usize, budget: Duration) {
    let deadline = Instant::now() + budget;
    while bus.tx.receiver_count() < want {
        if Instant::now() >= deadline {
            panic!(
                "only {} bus subscribers after {:?}; expected at least {}",
                bus.tx.receiver_count(),
                budget,
                want,
            );
        }
        tokio::task::yield_now().await;
    }
}

/// Connect to a TCP server, retrying until the listener is ready.
async fn connect_tcp_with_retry(addr: &str, budget: Duration) -> TcpStream {
    let deadline = Instant::now() + budget;
    loop {
        match TcpStream::connect(addr).await {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => tokio::task::yield_now().await,
            Err(e) => panic!("router tcp endpoint never became connectable at {addr}: {e}"),
        }
    }
}

/// Drain pending bytes from a TCP client, yielding between drains so any
/// broadcast echo still in the per-client writer task's queue lands
/// before we declare the socket quiet. A single drain pass is racy
/// against in-flight broadcast fanout — by the time `try_read` returns
/// `WouldBlock`, the next echo can be one task tick away from arriving.
async fn drain_socket(client: &mut TcpStream) {
    for _ in 0..3 {
        while client.try_read(&mut [0u8; 1024]).is_ok() {}
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }
    while client.try_read(&mut [0u8; 1024]).is_ok() {}
}

/// Wait until the routing table has learned at least `want` distinct
/// system IDs. The routing-update channel is `try_send` from the hot
/// path, so the table converges asynchronously after a heartbeat lands.
async fn wait_for_routing_systems(table: &Arc<RoutingTable>, want: usize, budget: Duration) {
    let deadline = Instant::now() + budget;
    while table.stats().total_systems < want {
        if Instant::now() >= deadline {
            panic!(
                "routing table only learned {} systems after {:?}; expected at least {}",
                table.stats().total_systems,
                budget,
                want,
            );
        }
        tokio::task::yield_now().await;
    }
}

/// Reserve `n` ephemeral TCP ports by binding temporary listeners on
/// 127.0.0.1:0, reading back the kernel-assigned ports, then dropping
/// the listeners so the router's real endpoints can bind them.
fn claim_tcp_ports(n: usize) -> Vec<u16> {
    let listeners: Vec<std::net::TcpListener> = (0..n)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("reserve tcp port"))
        .collect();
    let ports: Vec<u16> = listeners
        .iter()
        .map(|l| l.local_addr().expect("local_addr").port())
        .collect();
    drop(listeners);
    ports
}

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
async fn test_targeted_message_routing() {
    let ports = claim_tcp_ports(3);
    let port1 = ports[0];
    let port2 = ports[1];
    let port3 = ports[2];

    let toml = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port1}"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port2}"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port3}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml).await.expect("router should start");

    let mut client1 =
        connect_tcp_with_retry(&format!("127.0.0.1:{port1}"), Duration::from_secs(5)).await;
    let mut client2 =
        connect_tcp_with_retry(&format!("127.0.0.1:{port2}"), Duration::from_secs(5)).await;
    let mut client3 =
        connect_tcp_with_retry(&format!("127.0.0.1:{port3}"), Duration::from_secs(5)).await;

    // Each TCP-server endpoint forks a per-client subscriber on accept.
    wait_for_bus_subscribers(router.bus(), 3, Duration::from_secs(5)).await;

    client1.write_all(&heartbeat_bytes(1)).await.unwrap();
    client2.write_all(&heartbeat_bytes(2)).await.unwrap();
    client3.write_all(&heartbeat_bytes(3)).await.unwrap();

    // Routing-update channel is non-blocking try_send; wait for the updater
    // task to absorb the three heartbeats before issuing a targeted command.
    wait_for_routing_systems(router.routing_table(), 3, Duration::from_secs(5)).await;
    // Routing convergence races against the broadcast echo-fanout: by the
    // time the table reports 3 systems learned, the heartbeat broadcasts
    // may still be queued in the per-client writer tasks. Drain twice with
    // a yield gap so any in-flight echo lands before the second drain.
    drain_socket(&mut client1).await;
    drain_socket(&mut client2).await;
    drain_socket(&mut client3).await;

    client1.write_all(&command_long_bytes(2, 1)).await.unwrap();

    let (header, msg) = read_mavlink(&mut client2)
        .await
        .expect("client2 should receive the targeted command");
    assert_eq!(header.system_id, 1, "source is system 1");
    assert_eq!(msg.message_id(), 76, "COMMAND_LONG message id");

    // Negative assertion: client3 must NOT receive. The positive case above
    // (client2 received within its 1s read timeout) proves the bus has
    // already processed the COMMAND_LONG, so a single yield is enough to
    // surface any spurious broadcast that would have arrived on client3.
    tokio::task::yield_now().await;
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
async fn test_unknown_target_dropped() {
    let port = claim_tcp_ports(1)[0];

    let toml = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml).await.expect("router should start");

    let mut client =
        connect_tcp_with_retry(&format!("127.0.0.1:{port}"), Duration::from_secs(5)).await;

    // TCP-server forks a per-client subscriber on accept.
    wait_for_bus_subscribers(router.bus(), 1, Duration::from_secs(5)).await;

    client.write_all(&heartbeat_bytes(1)).await.unwrap();

    // Wait for the routing updater to record system 1.
    wait_for_routing_systems(router.routing_table(), 1, Duration::from_secs(5)).await;
    drain_socket(&mut client).await;

    // Send a command targeted at an unknown system (200). Because we're
    // the only endpoint and we don't claim to have seen system 200, the
    // router should drop it.
    client.write_all(&command_long_bytes(200, 1)).await.unwrap();

    // Negative assertion: nothing should arrive. There is no positive
    // counterpart here (no other endpoint to receive on), so we yield a few
    // times to give any spurious broadcast a chance to surface, then read
    // non-blockingly.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    if let Ok(n) = client.try_read(&mut [0u8; 1024]) {
        assert_eq!(n, 0, "message to unknown system should be dropped");
    }

    router.stop().await;
}
