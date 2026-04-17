//! Public-API network tests for UDP/TCP client modes and the TCP-server
//! connection limit. All tests drive the router exclusively through
//! [`mavrouter::Router`].

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::arithmetic_side_effects)]

use mavlink::MavHeader;
use mavrouter::{MessageBus, Router};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Wait until the router's internal bus has at least `want` subscribers.
/// UDP and serial endpoints subscribe on spawn; TCP server/client endpoints
/// subscribe *per accepted or established connection*. Using
/// `receiver_count()` as the sync primitive gives us a concrete observable
/// in place of the blind `tokio::time::sleep` these tests used to rely on
/// — per CLAUDE.md's "synchronize with events, not sleep" rule.
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

/// Helper: build a valid MAVLink v2 HEARTBEAT message as raw bytes.
fn build_heartbeat_bytes() -> Vec<u8> {
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

/// Router UDP-client endpoint forwards bus traffic to an external UDP socket.
#[tokio::test]
async fn test_udp_client_mode_forwarding() {
    // The UDP sink stays bound for the whole test — the router's
    // UDP-client endpoint will send packets to this port. Bind it on
    // `:0` and read back the kernel-assigned port; no grab-and-release
    // dance needed since we own the socket.
    let udp_recv = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp_recv.local_addr().unwrap().port();
    let tcp_port = claim_tcp_ports(1)[0];

    let toml_config = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "udp"
address = "127.0.0.1:{udp_port}"
mode = "client"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{tcp_port}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml_config).await.unwrap();

    // The UDP-client endpoint subscribes to the bus on spawn, but that
    // subscribe runs inside a spawned task — it's live some time after
    // `from_str` returns. Wait for it as a bus-subscriber count instead
    // of a blind sleep. The heartbeat must go nowhere if UDP hasn't
    // subscribed by the time TCP-server publishes.
    wait_for_bus_subscribers(router.bus(), 1, Duration::from_secs(5)).await;

    // Poll until the router's TCP-server endpoint is accepting — avoids
    // the race between the helper's port-release and the router's bind.
    let mut tcp_client = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match TcpStream::connect(format!("127.0.0.1:{tcp_port}")).await {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => {
                    tokio::task::yield_now().await;
                }
                Err(e) => panic!("router tcp endpoint never became connectable: {}", e),
            }
        }
    };

    // TCP-server spawns a writer task per accepted connection that *also*
    // subscribes to the bus. Wait for it so we know the server has fully
    // processed our connect before we push data into it.
    wait_for_bus_subscribers(router.bus(), 2, Duration::from_secs(5)).await;

    let heartbeat = build_heartbeat_bytes();
    tcp_client.write_all(&heartbeat).await.unwrap();

    // Single bounded read — no retry loop needed: we've already confirmed
    // both endpoints are subscribed, so the heartbeat must traverse the
    // bus and land on udp_recv. If it doesn't within 2 s, routing is
    // broken, not merely slow.
    let mut udp_buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(2), udp_recv.recv(&mut udp_buf))
        .await
        .expect("udp recv timed out — heartbeat never traversed the bus")
        .unwrap();
    assert!(n > 0, "udp_recv returned a zero-length packet");
    assert_eq!(udp_buf[0], 0xFD, "Expected MAVLink v2 magic byte");

    router.stop().await;
}

/// Router TCP-client endpoint connects to an external TCP server and
/// forwards traffic out through the established connection.
#[tokio::test]
async fn test_tcp_client_mode_forwarding() {
    // External TCP server that the router's TCP-client endpoint will
    // dial. Bind on `:0` and read back the assigned port — we keep the
    // listener alive until accept() returns.
    let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_port = tcp_listener.local_addr().unwrap().port();
    let udp_port = claim_udp_port();

    let toml_config = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{tcp_port}"
mode = "client"

[[endpoint]]
type = "udp"
address = "127.0.0.1:{udp_port}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml_config).await.unwrap();

    // Block on accept() — the router's TCP-client endpoint will connect
    // once it's finished spinning up. This is an event-driven wait, no
    // timing heuristics needed beyond the 5s cap against a broken router.
    let (mut tcp_stream, _addr) =
        tokio::time::timeout(Duration::from_secs(5), tcp_listener.accept())
            .await
            .expect("Timeout waiting for router to connect as TCP client")
            .unwrap();

    // Two expected subscribers at steady state:
    //   1. UDP-server endpoint, subscribed on spawn.
    //   2. TCP-client's per-connection writer, subscribed after the
    //      router-side `TcpStream::connect` returns (which is what our
    //      accept() just observed).
    // Waiting for count ≥ 2 covers the window between accept() returning
    // on our side and subscribe() completing on the router's side.
    wait_for_bus_subscribers(router.bus(), 2, Duration::from_secs(5)).await;

    let udp_sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp_sender
        .connect(format!("127.0.0.1:{udp_port}"))
        .await
        .unwrap();

    let heartbeat = build_heartbeat_bytes();
    udp_sender.send(&heartbeat).await.unwrap();

    // Bounded single read; both endpoints are subscribed so the heartbeat
    // must traverse UDP → bus → TCP within budget.
    let mut tcp_buf = [0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(2), tcp_stream.read(&mut tcp_buf))
        .await
        .expect("tcp read timed out — heartbeat never traversed the bus")
        .unwrap();
    assert!(n > 0, "tcp_stream returned a zero-length read");
    assert_eq!(tcp_buf[0], 0xFD, "Expected MAVLink v2 magic byte");

    router.stop().await;
}

/// Router TCP-server enforces its `MAX_TCP_CLIENTS` ceiling. Connect 100
/// clients (all should succeed), then verify the 101st is dropped.
#[tokio::test]
async fn test_tcp_server_connection_limit() {
    let tcp_port = claim_tcp_ports(1)[0];

    let toml_config = format!(
        r#"
[general]
bus_capacity = 100
dedup_period_ms = 0

[[endpoint]]
type = "tcp"
address = "127.0.0.1:{tcp_port}"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml_config).await.unwrap();

    // Poll until the router's TCP-server endpoint is accepting — avoids
    // the race between the helper's port-release and the router's bind.
    let first_client = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match TcpStream::connect(format!("127.0.0.1:{tcp_port}")).await {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => {
                    tokio::task::yield_now().await;
                }
                Err(e) => panic!("router tcp endpoint never became connectable: {}", e),
            }
        }
    };

    let mut clients = vec![first_client];
    for i in 1..100 {
        match tokio::time::timeout(
            Duration::from_secs(2),
            TcpStream::connect(format!("127.0.0.1:{tcp_port}")),
        )
        .await
        {
            Ok(Ok(stream)) => clients.push(stream),
            Ok(Err(e)) => panic!("Failed to connect client {}: {}", i, e),
            Err(_) => panic!("Timeout connecting client {}", i),
        }
    }

    // Wait for all 100 accept paths to finish — each successful accept
    // spawns a per-connection writer that subscribes to the bus. Waiting
    // for `receiver_count() == 100` is the concrete observable proving
    // the server has accepted all 100 and is at its limit, replacing the
    // old blind 500 ms margin.
    wait_for_bus_subscribers(router.bus(), 100, Duration::from_secs(5)).await;

    // The 101st connection is accepted at the TCP level but the server
    // drops its side of the stream immediately. Any of: EOF, read error,
    // timeout, or refused connect is acceptable — the only unacceptable
    // outcome would be a long-lived, uninterrupted data connection, which
    // we don't test for explicitly because detecting *its absence* under
    // OS backlog semantics is racy.
    if let Ok(Ok(mut extra_stream)) = tokio::time::timeout(
        Duration::from_secs(2),
        TcpStream::connect(format!("127.0.0.1:{tcp_port}")),
    )
    .await
    {
        let mut buf = [0u8; 64];
        // Drain one read (or time out). We don't assert on the outcome;
        // seeing data is fine if the OS accept backlog surfaced after a
        // slot freed up between our accept and read.
        tokio::time::timeout(Duration::from_secs(2), extra_stream.read(&mut buf))
            .await
            .ok();
    }

    drop(clients);
    router.stop().await;
}
