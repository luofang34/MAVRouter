//! UDP endpoint for MAVLink communications.
//!
//! This module handles MAVLink traffic over UDP sockets, supporting both
//! server (binding to an address and listening for incoming packets)
//! and client (sending to a specific remote address) modes.
//!
//! In server mode, it tracks connected clients by their `SocketAddr` to allow
//! broadcasting messages back to them. In client mode, it continuously sends
//! to a predefined remote address.
//!
//! **Broadcast mode** (client mode with a broadcast target address):
//! When the target address is a broadcast address (e.g., 192.168.1.255 or
//! 255.255.255.255), the endpoint starts by sending to the broadcast address.
//! Once a unicast peer responds, it switches to sending directly to that peer.
//! If the peer is silent for `broadcast_timeout_secs`, it reverts to broadcast.
//! This allows GCS changes without reconfiguration.

use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::{EndpointCore, EndpointStats};
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::framing::MavlinkFrame;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use bytes::Bytes;
use dashmap::DashMap;
use tokio::sync::broadcast::{self, error::RecvError};
// futures::join_all removed - sequential sends avoid Vec allocation (issue #17)
use mavlink::MavlinkVersion;
use parking_lot::{Mutex, RwLock};
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

/// Returns the appropriate local bind address for a UDP client based on the target address family.
fn client_bind_addr(target: &SocketAddr) -> &'static str {
    if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    }
}

/// Checks whether a `SocketAddr` is a broadcast address.
///
/// Returns `true` for IPv4 addresses where the host part is all 1s:
/// - `255.255.255.255` (global broadcast)
/// - Addresses ending in `.255` (common subnet broadcast for /24 networks)
///
/// Returns `false` for all IPv6 addresses (IPv6 uses multicast, not broadcast).
pub fn is_broadcast_addr(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ipv4) => ipv4 == Ipv4Addr::BROADCAST || ipv4.octets()[3] == 255,
        IpAddr::V6(_) => false,
    }
}

/// Runs the UDP endpoint logic, continuously handling MAVLink traffic.
///
/// This function sets up a UDP socket based on the specified mode (server or client)
/// and manages incoming and outgoing MAVLink messages through the provided message bus.
///
/// In **server mode**, it binds to the given `address` and tracks all unique `SocketAddr`s
/// from which it receives messages, using them as targets for outgoing broadcast messages.
/// In **client mode**, it continuously sends outgoing messages to the specified `address`.
///
/// In **broadcast client mode** (client mode with a broadcast target address), it sends
/// to the broadcast address until a unicast peer responds, then switches to unicast.
/// If the peer is silent for `broadcast_timeout_secs`, it reverts to broadcast.
///
/// # Arguments
///
/// * `id` - Unique identifier for this endpoint.
/// * `address` - The UDP address to bind to (server) or send to (client), e.g., "0.0.0.0:14550" or "127.0.0.1:14550".
/// * `mode` - The operating mode (`EndpointMode::Server` or `EndpointMode::Client`).
/// * `bus_tx` - Sender half of the message bus for sending `RoutedMessage`s to other endpoints.
/// * `bus_rx` - Receiver half of the message bus for receiving `RoutedMessage`s from other endpoints.
/// * `routing_table` - Shared `RoutingTable` to update and query routing information.
/// * `dedup` - Shared `Dedup` instance for message deduplication.
/// * `filters` - `EndpointFilters` to apply for this specific endpoint.
/// * `token` - `CancellationToken` to signal graceful shutdown.
/// * `cleanup_ttl_secs` - Time-to-live for stale client entries in server mode.
/// * `broadcast_timeout_secs` - Timeout before reverting from unicast to broadcast in broadcast client mode.
///
/// # Returns
///
/// A `Result` indicating success or failure. The function will run indefinitely
/// until the `CancellationToken` is cancelled or a critical error occurs.
///
/// # Errors
///
/// Returns an `anyhow::Error` if:
/// - Binding to the specified address fails.
/// - The remote address for client mode cannot be resolved.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    address: String,
    mode: crate::config::EndpointMode,
    bus_tx: broadcast::Sender<RoutedMessage>,
    bus_rx: broadcast::Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: ConcurrentDedup,
    filters: EndpointFilters,
    token: CancellationToken,
    cleanup_ttl_secs: u64,
    broadcast_timeout_secs: u64,
    stats: Arc<EndpointStats>,
) -> Result<()> {
    let core = EndpointCore {
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
        update_routing: true,
        stats,
    };

    let (bind_addr, target_addr) = if mode == crate::config::EndpointMode::Server {
        (address.clone(), None)
    } else {
        let mut addrs = address.to_socket_addrs().map_err(|e| {
            RouterError::config(format!("Invalid remote address '{}': {}", address, e))
        })?;
        let target = addrs.next().ok_or_else(|| {
            RouterError::config(format!("Could not resolve remote address '{}'", address))
        })?;
        (client_bind_addr(&target).to_string(), Some(target))
    };

    let socket = UdpSocket::bind(&bind_addr)
        .await
        .map_err(|e| RouterError::network(&bind_addr, e))?;

    // Enable broadcast on the socket if the target is a broadcast address
    let broadcast_mode = target_addr.as_ref().is_some_and(is_broadcast_addr);
    if broadcast_mode {
        socket
            .set_broadcast(true)
            .map_err(|e| RouterError::network(&address, e))?;
        info!(
            "UDP endpoint {} entering broadcast mode for {}",
            id, address
        );
    }

    // Peer state for broadcast mode: tracks the current unicast peer and when it was last seen
    let broadcast_peer: Arc<Mutex<Option<(SocketAddr, Instant)>>> = Arc::new(Mutex::new(None));

    let r = Arc::new(socket);
    let s = r.clone();

    // Map of client address to last seen time
    let clients: Arc<DashMap<SocketAddr, Instant>> = Arc::new(DashMap::new());

    let clients_recv = clients.clone();
    let r_socket = r.clone();
    let core_rx = core.clone();
    let broadcast_peer_recv = broadcast_peer.clone();
    let broadcast_addr_for_recv = target_addr;

    // Receiver Task
    let recv_loop = async move {
        let mut buf = [0u8; 65535];
        loop {
            match r_socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    clients_recv.insert(addr, Instant::now());

                    // In broadcast mode, track unicast peers
                    if broadcast_mode {
                        if let Some(bcast_addr) = broadcast_addr_for_recv {
                            if addr != bcast_addr && !is_broadcast_addr(&addr) {
                                let mut peer = broadcast_peer_recv.lock();
                                let is_new = peer
                                    .as_ref()
                                    .map_or(true, |(prev_addr, _)| *prev_addr != addr);
                                if is_new {
                                    info!(
                                        "UDP broadcast endpoint: switching to unicast peer {}",
                                        addr
                                    );
                                }
                                *peer = Some((addr, Instant::now()));
                            }
                        }
                    }

                    let mut cursor = Cursor::new(&buf[..len]);
                    let res = mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut cursor)
                        .map(|(h, m)| (h, m, MavlinkVersion::V2))
                        .or_else(|_| {
                            cursor.set_position(0);
                            mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut cursor)
                                .map(|(h, m)| (h, m, MavlinkVersion::V1))
                        });

                    if let Ok((header, message, version)) = res {
                        // Capture raw bytes from UDP datagram (zero-copy)
                        let packet_len = cursor.position() as usize;
                        let raw_bytes = Bytes::copy_from_slice(&buf[..packet_len]);
                        // Use Core logic
                        core_rx.handle_incoming_frame(MavlinkFrame {
                            header,
                            message,
                            version,
                            raw_bytes,
                        });
                    } else {
                        trace!(
                            "UDP: Failed to parse MAVLink packet from {} ({} bytes)",
                            addr,
                            len
                        );
                    }
                }
                Err(e) => {
                    core_rx.stats.errors.fetch_add(1, Ordering::Relaxed);
                    error!("UDP recv error: {}", e);
                    // Backoff to prevent tight error loop (issue #21)
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    };

    // Sender Task
    let clients_send = clients.clone();
    let s_socket = s.clone();
    let mut bus_rx_loop = bus_rx;
    let core_tx = core.clone();
    let broadcast_peer_send = broadcast_peer.clone();
    let broadcast_timeout = Duration::from_secs(broadcast_timeout_secs);

    let send_loop = async move {
        // Pre-allocated buffer for client addresses to avoid per-message allocation (issue #17)
        let mut targets: Vec<SocketAddr> = Vec::new();

        loop {
            match bus_rx_loop.recv().await {
                Ok(msg) => {
                    if !core_tx.check_outgoing(&msg) {
                        continue;
                    }

                    let packet_data = msg.serialized_bytes.clone();
                    let pkt_len = packet_data.len() as u64;

                    if let Some(target) = target_addr {
                        if broadcast_mode {
                            // Broadcast mode: check if we have an active unicast peer
                            let send_addr = {
                                let peer = broadcast_peer_send.lock();
                                match &*peer {
                                    Some((addr, last_seen))
                                        if last_seen.elapsed() < broadcast_timeout =>
                                    {
                                        *addr
                                    }
                                    _ => target,
                                }
                            };
                            match s_socket.send_to(&packet_data, send_addr).await {
                                Ok(_) => {
                                    core_tx.stats.record_outgoing(pkt_len);
                                }
                                Err(e) => {
                                    core_tx.stats.errors.fetch_add(1, Ordering::Relaxed);
                                    warn!("UDP send error to {}: {}", send_addr, e);
                                }
                            }
                        } else {
                            // Normal client mode: always send to target
                            match s_socket.send_to(&packet_data, target).await {
                                Ok(_) => {
                                    core_tx.stats.record_outgoing(pkt_len);
                                }
                                Err(e) => {
                                    core_tx.stats.errors.fetch_add(1, Ordering::Relaxed);
                                    warn!("UDP send error to target: {}", e);
                                }
                            }
                        }
                    } else {
                        // Collect keys first (releases DashMap shard locks before await)
                        targets.clear();
                        targets.extend(clients_send.iter().map(|r| *r.key()));

                        for target in &targets {
                            match s_socket.send_to(&packet_data, target).await {
                                Ok(_) => {
                                    core_tx.stats.record_outgoing(pkt_len);
                                }
                                Err(e) => {
                                    core_tx.stats.errors.fetch_add(1, Ordering::Relaxed);
                                    warn!("UDP broadcast error to {}: {}", target, e);
                                }
                            }
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!("UDP Sender lagged: missed {} messages", n);
                }
                Err(RecvError::Closed) => break,
            }
        }
    };

    // Cleanup Task (runs every 60s, removes clients inactive for > 300s)
    let clients_cleanup = clients.clone();
    let cleanup_loop = async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let now = Instant::now();
            let ttl = Duration::from_secs(cleanup_ttl_secs);
            let initial_len = clients_cleanup.len();
            clients_cleanup.retain(|_addr, last_seen| now.duration_since(*last_seen) < ttl);
            let dropped = initial_len - clients_cleanup.len();
            if dropped > 0 {
                debug!("UDP Cleanup: removed {} stale clients", dropped);
            }
        }
    };

    tokio::select! {
        _ = recv_loop => Ok(()),
        _ = send_loop => Ok(()),
        _ = cleanup_loop => Ok(()),
        _ = token.cancelled() => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

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
        // Simulate the broadcast peer state transitions
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
        // Try to bind to IPv6 - skip test if not supported
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
}
