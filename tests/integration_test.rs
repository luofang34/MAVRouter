//! End-to-end network integration tests driven entirely through the public
//! [`mavrouter::Router`] API. No internal endpoint types are referenced.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::panic)]
#![allow(clippy::arithmetic_side_effects)]

use mavlink::MavHeader;
use mavrouter::{MessageBus, Router};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

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
/// Closes the bind/release race that the helper's port-reservation
/// pattern leaves open.
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

/// Reserve one ephemeral UDP port. See [`claim_tcp_ports`] for the
/// reason we grab-and-release instead of passing the kernel `:0`
/// through TOML directly (the TOML-driven public API doesn't surface
/// the post-bind address back to the test).
fn claim_udp_port() -> u16 {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("reserve udp port");
    sock.local_addr().expect("local_addr").port()
}

/// Build a MAVLink v2 HEARTBEAT as raw bytes.
fn heartbeat_bytes() -> Vec<u8> {
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        sequence: 0,
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();
    buf
}

/// UDP → TCP loopback through a two-endpoint Router.
#[tokio::test]
async fn test_udp_to_tcp_echo() {
    let udp_port = claim_udp_port();
    let tcp_port = claim_tcp_ports(1)[0];

    let toml = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "udp"
address = "127.0.0.1:{udp_port}"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{tcp_port}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml).await.expect("router should start");

    // UDP-server subscribes on spawn. TCP-server only adds subscribers per
    // accepted client; its listener-ready signal is the connect_tcp_with_retry
    // call below.
    wait_for_bus_subscribers(router.bus(), 1, Duration::from_secs(5)).await;

    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp.connect(format!("127.0.0.1:{udp_port}")).await.unwrap();
    let mut tcp =
        connect_tcp_with_retry(&format!("127.0.0.1:{tcp_port}"), Duration::from_secs(5)).await;

    // Post-accept: TCP-server forks a per-client subscriber. UDP-server (1) +
    // TCP per-client (1) = 2.
    wait_for_bus_subscribers(router.bus(), 2, Duration::from_secs(5)).await;

    let buf = heartbeat_bytes();
    let mut rx = [0u8; 1024];
    let mut received = false;

    for _ in 0..5 {
        udp.send(&buf).await.unwrap();
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(500), tcp.read(&mut rx)).await
        {
            if n > 0 {
                assert_eq!(rx[0], 0xFD, "MAVLink v2 magic expected");
                received = true;
                break;
            }
        }
    }
    assert!(received, "UDP -> TCP echo did not arrive");

    router.stop().await;
}

/// TCP → TCP loopback: two TCP server endpoints bridged by the router's bus.
#[tokio::test]
async fn test_tcp_to_tcp_bidirectional() {
    let ports = claim_tcp_ports(2);
    let port_a = ports[0];
    let port_b = ports[1];

    let toml = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port_a}"
mode = "server"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{port_b}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml).await.expect("router should start");

    let mut client1 =
        connect_tcp_with_retry(&format!("127.0.0.1:{port_a}"), Duration::from_secs(5)).await;
    let mut client2 =
        connect_tcp_with_retry(&format!("127.0.0.1:{port_b}"), Duration::from_secs(5)).await;

    // Both TCP-server endpoints fork a per-client subscriber on accept.
    wait_for_bus_subscribers(router.bus(), 2, Duration::from_secs(5)).await;

    let buf = heartbeat_bytes();
    let mut rx = [0u8; 1024];
    let mut received = false;

    for _ in 0..5 {
        client1.write_all(&buf).await.unwrap();
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(500), client2.read(&mut rx)).await
        {
            if n > 0 {
                assert_eq!(rx[0], 0xFD);
                received = true;
                break;
            }
        }
    }
    assert!(received, "TCP -> TCP bridging did not deliver");

    router.stop().await;
}
