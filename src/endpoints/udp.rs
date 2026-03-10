//! UDP endpoint for MAVLink communications.
//!
//! This module handles MAVLink traffic over UDP sockets, supporting both
//! server (binding to an address and listening for incoming packets)
//! and client (sending to a specific remote address) modes.
//!
//! In server mode, it tracks connected clients by their `SocketAddr` to allow
//! broadcasting messages back to them. In client mode, it continuously sends
//! to a predefined remote address.

use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::EndpointCore;
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::framing::MavlinkFrame;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use async_broadcast::{Receiver, RecvError, Sender};
use bytes::Bytes;
use dashmap::DashMap;
use mavlink::MavlinkVersion;
use parking_lot::RwLock;
use std::io::Cursor;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Sends packet data to either a specific target or all known clients.
async fn send_to_targets(
    socket: &UdpSocket,
    data: &[u8],
    target: Option<&SocketAddr>,
    clients: &[SocketAddr],
) {
    if let Some(target) = target {
        if let Err(e) = socket.send_to(data, target).await {
            warn!("UDP send error to target: {}", e);
        }
    } else {
        for client in clients {
            if let Err(e) = socket.send_to(data, client).await {
                warn!("UDP broadcast error to {}: {}", client, e);
            }
        }
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
    bus_tx: Sender<RoutedMessage>,
    bus_rx: Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: ConcurrentDedup,
    filters: EndpointFilters,
    token: CancellationToken,
    cleanup_ttl_secs: u64,
) -> Result<()> {
    let core = EndpointCore {
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
        update_routing: true,
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
        ("0.0.0.0:0".to_string(), Some(target))
    };

    let socket = UdpSocket::bind(&bind_addr)
        .await
        .map_err(|e| RouterError::network(&bind_addr, e))?;

    let r = Arc::new(socket);
    let s = r.clone();

    // Map of client address to last seen time
    let clients: Arc<DashMap<SocketAddr, Instant>> = Arc::new(DashMap::new());

    let clients_recv = clients.clone();
    let r_socket = r.clone();
    let core_rx = core.clone();

    // Receiver Task
    let recv_loop = async move {
        let mut buf = [0u8; 65535];
        loop {
            match r_socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    clients_recv.insert(addr, Instant::now());

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
                    }
                }
                Err(e) => {
                    error!("UDP recv error: {}", e);
                }
            }
        }
    };

    // Sender Task
    let clients_send = clients.clone();
    let s_socket = s.clone();
    let mut bus_rx_loop = bus_rx;
    let core_tx = core.clone();

    let send_loop = async move {
        // Reusable buffer for client snapshot to avoid per-message allocation
        let mut cached_targets: Vec<SocketAddr> = Vec::new();

        loop {
            match bus_rx_loop.recv().await {
                Ok(msg) => {
                    // Snapshot client list once per batch (reuse allocation)
                    if target_addr.is_none() {
                        cached_targets.clear();
                        cached_targets.extend(clients_send.iter().map(|r| *r.key()));
                    }

                    // Send first message
                    if core_tx.check_outgoing(&msg) {
                        send_to_targets(&s_socket, &msg.serialized_bytes, target_addr.as_ref(), &cached_targets).await;
                    }

                    // Batch: drain pending messages without blocking
                    for _ in 0..1024 {
                        match bus_rx_loop.try_recv() {
                            Ok(m) => {
                                if core_tx.check_outgoing(&m) {
                                    send_to_targets(&s_socket, &m.serialized_bytes, target_addr.as_ref(), &cached_targets).await;
                                }
                            }
                            Err(async_broadcast::TryRecvError::Empty) => break,
                            Err(async_broadcast::TryRecvError::Overflowed(n)) => {
                                warn!("UDP Sender lagged: missed {} messages", n);
                            }
                            Err(async_broadcast::TryRecvError::Closed) => return,
                        }
                    }
                }
                Err(RecvError::Overflowed(n)) => {
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
