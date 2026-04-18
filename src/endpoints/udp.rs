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

use crate::endpoint_core::{recv_next_bus_msg, EndpointSpawnContext};
use crate::error::{Result, RouterError};
use crate::framing::MavlinkFrame;
use bytes::Bytes;
use dashmap::DashMap;
// futures::join_all removed - sequential sends avoid Vec allocation (issue #17)
use mavlink::MavlinkVersion;
use parking_lot::Mutex;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
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
/// * `ctx` - Shared spawn context (bus, routing table, dedup, filters, stats,
///   cancellation token).
/// * `address` - The UDP address to bind to (server) or send to (client),
///   e.g. `"0.0.0.0:14550"` or `"127.0.0.1:14550"`.
/// * `mode` - The operating mode ([`crate::config::EndpointMode::Server`] or
///   [`crate::config::EndpointMode::Client`]).
/// * `cleanup_ttl_secs` - Time-to-live for stale client entries in server mode.
/// * `broadcast_timeout_secs` - Timeout before reverting from unicast to
///   broadcast in broadcast client mode.
///
/// # Returns
///
/// A `Result` indicating success or failure. The function will run indefinitely
/// until the cancellation token in `ctx` is cancelled or a critical error occurs.
///
/// # Errors
///
/// Returns a [`crate::error::RouterError`] if:
/// - Binding to the specified address fails.
/// - The remote address for client mode cannot be resolved.
pub async fn run(
    ctx: EndpointSpawnContext,
    address: String,
    mode: crate::config::EndpointMode,
    cleanup_ttl_secs: u64,
    broadcast_timeout_secs: u64,
) -> Result<()> {
    let id = ctx.id;
    let token = ctx.cancel_token.clone();
    let bus_rx = ctx.subscribe_bus();
    let core = ctx.endpoint_core(true);

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

                    // len comes from socket recv, always <= buf.len()
                    #[allow(clippy::indexing_slicing)]
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
                        #[allow(clippy::cast_possible_truncation)]
                        let packet_len = cursor.position() as usize;
                        // packet_len <= len <= buf.len()
                        #[allow(clippy::indexing_slicing)]
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

    let send_loop_name = format!("UDP Sender {}", id);
    let send_loop = async move {
        // Pre-allocated buffer for client addresses to avoid per-message allocation (issue #17)
        let mut targets: Vec<SocketAddr> = Vec::new();

        loop {
            // `recv_next_bus_msg` centralises the `Lagged`/`Closed` bookkeeping
            // shared with every other transport's send loop — counting drops
            // and continuing on `Lagged`, breaking on `Closed`.
            let Some(msg) =
                recv_next_bus_msg(&mut bus_rx_loop, &core_tx.stats, &send_loop_name).await
            else {
                break;
            };
            if !core_tx.check_outgoing(&msg) {
                continue;
            }

            let packet_data = msg.serialized_bytes.clone();
            #[allow(clippy::cast_possible_truncation)] // MAVLink frame fits u64
            let pkt_len = packet_data.len() as u64;

            if let Some(target) = target_addr {
                if broadcast_mode {
                    // Broadcast mode: check if we have an active unicast peer
                    let send_addr = {
                        let peer = broadcast_peer_send.lock();
                        match &*peer {
                            Some((addr, last_seen)) if last_seen.elapsed() < broadcast_timeout => {
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
            let dropped = initial_len.saturating_sub(clients_cleanup.len());
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
mod tests;
