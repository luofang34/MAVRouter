//! Unit tests for the UDP endpoint.
//!
//! Extracted from `src/endpoints/udp.rs` once the inline test module
//! grew past the 500-line size budget. The Lagged-counter assertion
//! sits here so the production file stays under budget as the test
//! suite grows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

use super::*;
use crate::endpoint_core::EndpointStats;
use crate::mavlink_utils::MessageTarget;
use crate::router::{create_bus, EndpointId, RoutedMessage};
use bytes::Bytes;
use mavlink::{common::MavMessage, MavHeader, MavlinkVersion, Message};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[test]
fn test_is_broadcast_addr_global() {
    let addr: SocketAddr = "255.255.255.255:14550".parse().unwrap();
    assert!(is_broadcast_addr(&addr));
}

#[test]
fn test_is_broadcast_addr_subnet() {
    let addr: SocketAddr = "192.168.1.255:14550".parse().unwrap();
    assert!(is_broadcast_addr(&addr));
}

#[test]
fn test_is_broadcast_addr_unicast() {
    let addr: SocketAddr = "192.168.1.1:14550".parse().unwrap();
    assert!(!is_broadcast_addr(&addr));
}

#[test]
fn test_is_broadcast_addr_ipv6() {
    let addr: SocketAddr = "[::1]:14550".parse().unwrap();
    assert!(!is_broadcast_addr(&addr));
}

#[test]
fn test_is_broadcast_addr_10_network() {
    let addr: SocketAddr = "10.0.0.255:14550".parse().unwrap();
    assert!(is_broadcast_addr(&addr));
}

#[test]
fn test_broadcast_peer_state_machine() {
    let peer: Arc<Mutex<Option<(SocketAddr, Instant)>>> = Arc::new(Mutex::new(None));
    let broadcast_addr: SocketAddr = "192.168.1.255:14550".parse().unwrap();
    let unicast_addr: SocketAddr = "192.168.1.42:14550".parse().unwrap();
    let timeout = Duration::from_millis(100);

    // Initially no peer -> send to broadcast
    {
        let p = peer.lock();
        let send_addr = match &*p {
            Some((addr, last_seen)) if last_seen.elapsed() < timeout => *addr,
            _ => broadcast_addr,
        };
        assert_eq!(send_addr, broadcast_addr);
    }

    // Simulate receiving from unicast peer
    {
        let mut p = peer.lock();
        *p = Some((unicast_addr, Instant::now()));
    }

    // Now should send to unicast peer
    {
        let p = peer.lock();
        let send_addr = match &*p {
            Some((addr, last_seen)) if last_seen.elapsed() < timeout => *addr,
            _ => broadcast_addr,
        };
        assert_eq!(send_addr, unicast_addr);
    }

    // Simulate timeout by setting last_seen in the past
    {
        let mut p = peer.lock();
        *p = Some((unicast_addr, Instant::now() - Duration::from_millis(200)));
    }

    // After timeout, should revert to broadcast
    {
        let p = peer.lock();
        let send_addr = match &*p {
            Some((addr, last_seen)) if last_seen.elapsed() < timeout => *addr,
            _ => broadcast_addr,
        };
        assert_eq!(send_addr, broadcast_addr);
    }
}

#[test]
fn test_client_bind_addr_ipv4() {
    let target: SocketAddr = "192.168.1.1:14550".parse().unwrap();
    assert_eq!(client_bind_addr(&target), "0.0.0.0:0");
}

#[test]
fn test_client_bind_addr_ipv6() {
    let target: SocketAddr = "[::1]:14550".parse().unwrap();
    assert_eq!(client_bind_addr(&target), "[::]:0");
}

#[test]
fn test_ipv4_target_detection() {
    let addr: SocketAddr = "127.0.0.1:14550".parse().unwrap();
    assert!(addr.is_ipv4());
}

#[test]
fn test_ipv6_target_detection() {
    let addr: SocketAddr = "[::1]:14550".parse().unwrap();
    assert!(addr.is_ipv6());
}

#[test]
fn test_is_broadcast_addr_ipv6_not_broadcast() {
    let addr: SocketAddr = "[fe80::1]:14550".parse().unwrap();
    assert!(!is_broadcast_addr(&addr));
}

#[tokio::test]
async fn test_udp_ipv6_bind() {
    match tokio::net::UdpSocket::bind("[::]:0").await {
        Ok(sock) => {
            let addr = sock.local_addr().unwrap();
            assert!(addr.is_ipv6());
        }
        Err(_) => {
            // IPv6 not available in this environment, skip
        }
    }
}

// ============================================================================
// Lagged-event counter assertion — matches the pattern enforced in
// endpoint_core::tests::lagged for the stream_loop path. The UDP sender
// subscribes to the same bus broadcast channel and must treat
// `RecvError::Lagged(n)` as a counted correctness signal.
// ============================================================================

fn build_test_frame(source_id: EndpointId) -> RoutedMessage {
    let header = MavHeader {
        system_id: 42,
        component_id: 1,
        sequence: 0,
    };
    let mav_msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut frame = Vec::new();
    mavlink::write_v2_msg(&mut frame, header, &mav_msg).unwrap();
    RoutedMessage {
        source_id,
        header,
        message_id: mav_msg.message_id(),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(frame),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    }
}

/// Flood a capacity-2 bus before the UDP endpoint is ever spawned so
/// the receiver cursor is still at zero. The sender's first `recv()`
/// returns one big `Lagged(n)`, which must bump
/// `EndpointStats::bus_lagged`.
#[tokio::test]
async fn udp_sender_counts_lagged_from_bus() {
    use crate::dedup::ConcurrentDedup;
    use crate::filter::EndpointFilters;
    use crate::routing::RoutingTable;

    let bus = create_bus(2);
    let routing_table = Arc::new(RoutingTable::new());
    let (route_tx, _route_rx) = tokio::sync::mpsc::channel(16);
    let stats = Arc::new(EndpointStats::new());
    let cancel = CancellationToken::new();

    let bus_tx = bus.sender();
    // Subscribe BEFORE flooding — broadcast receivers anchor their cursor
    // at subscribe time, so sends after this point will overflow past the
    // receiver and produce a single `Lagged(n)` on its first `recv()`.
    let bus_rx = bus.subscribe();

    const FLOOD_COUNT: usize = 1000;
    // source_id differs from the endpoint id below, so messages aren't
    // self-filtered by EndpointCore::check_outgoing.
    let template = build_test_frame(EndpointId(99));
    for _ in 0..FLOOD_COUNT {
        bus.tx.send(Arc::new(template.clone())).ok();
    }
    let endpoint_stats = stats.clone();
    let endpoint_cancel = cancel.clone();
    let endpoint_routing = routing_table.clone();
    let endpoint_dedup = ConcurrentDedup::new(Duration::from_millis(100));
    let handle = tokio::spawn(async move {
        run(
            0,
            "127.0.0.1:0".to_string(),
            crate::config::EndpointMode::Server,
            bus_tx,
            bus_rx,
            endpoint_routing,
            route_tx,
            endpoint_dedup,
            EndpointFilters::default(),
            endpoint_cancel,
            60,
            60,
            endpoint_stats,
        )
        .await
    });

    // Poll until the counter reflects the lag.
    let deadline = Instant::now() + Duration::from_secs(3);
    while stats.bus_lagged.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .ok();

    let lagged = stats.bus_lagged.load(Ordering::Relaxed);
    assert!(
        lagged > 0,
        "expected bus_lagged > 0 after flooding {} messages into a capacity-2 bus, got {}",
        FLOOD_COUNT,
        lagged
    );
    assert_eq!(stats.snapshot().bus_lagged, lagged);
}
