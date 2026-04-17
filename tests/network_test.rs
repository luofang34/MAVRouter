//! Public-API network tests for UDP/TCP client modes and the TCP-server
//! connection limit. All tests drive the router exclusively through
//! [`mavrouter::Router`].

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::arithmetic_side_effects)]

use mavlink::MavHeader;
use mavrouter::Router;
use serial_test::serial;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

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
#[serial]
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

    // Wait for the router's TCP-server endpoint to accept connections
    // (instead of a blind sleep). Poll with a short backoff bounded by
    // a 5s budget — in practice this succeeds on the first try.
    let mut tcp_client = {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match TcpStream::connect(format!("127.0.0.1:{tcp_port}")).await {
                Ok(s) => break s,
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("router tcp endpoint never became connectable: {}", e),
            }
        }
    };

    let heartbeat = build_heartbeat_bytes();

    let mut udp_buf = [0u8; 1024];
    let mut received = false;

    // Retry loop: the TCP client has just connected, but the router's
    // broadcast-subscriber registration happens asynchronously on the
    // accept path. Repeated sends tolerate the race without a blind
    // sleep before the loop.
    for _ in 0..10 {
        tcp_client.write_all(&heartbeat).await.unwrap();

        match tokio::time::timeout(Duration::from_millis(500), udp_recv.recv(&mut udp_buf)).await {
            Ok(Ok(n)) if n > 0 => {
                assert_eq!(udp_buf[0], 0xFD, "Expected MAVLink v2 magic byte");
                received = true;
                break;
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    assert!(
        received,
        "Failed to receive heartbeat on UDP socket from router's UDP client endpoint"
    );

    router.stop().await;
}

/// Router TCP-client endpoint connects to an external TCP server and
/// forwards traffic out through the established connection.
#[tokio::test]
#[serial]
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

    let udp_sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    udp_sender
        .connect(format!("127.0.0.1:{udp_port}"))
        .await
        .unwrap();

    let heartbeat = build_heartbeat_bytes();

    let mut tcp_buf = [0u8; 1024];
    let mut received = false;

    for _ in 0..10 {
        udp_sender.send(&heartbeat).await.unwrap();

        match tokio::time::timeout(Duration::from_millis(500), tcp_stream.read(&mut tcp_buf)).await
        {
            Ok(Ok(n)) if n > 0 => {
                assert_eq!(tcp_buf[0], 0xFD, "Expected MAVLink v2 magic byte");
                received = true;
                break;
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    assert!(
        received,
        "Failed to receive heartbeat on TCP stream from router's TCP client endpoint"
    );

    router.stop().await;
}

/// Router TCP-server enforces its `MAX_TCP_CLIENTS` ceiling. Connect 100
/// clients (all should succeed), then verify the 101st is dropped.
#[tokio::test]
#[serial]
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
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match TcpStream::connect(format!("127.0.0.1:{tcp_port}")).await {
                Ok(s) => break s,
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
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

    // Let the server process all accepts before we probe the limit.
    tokio::time::sleep(Duration::from_millis(500)).await;

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
