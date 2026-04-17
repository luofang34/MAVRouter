//! TCP endpoint for MAVLink connections.
//!
//! This module provides functionality to establish and manage TCP connections
//! for MAVLink communication, supporting both server and client modes.
//!
//! In **server mode**, it listens for incoming TCP connections and handles
//! each client in a separate task.
//! In **client mode**, it attempts to connect to a remote TCP server and
//! automatically retries connection if lost.

use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::{run_stream_loop, EndpointCore, EndpointStats, ExponentialBackoff};
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Global counter for TCP client endpoint IDs, starting high enough
/// to avoid collision with configured endpoint IDs (issue #6).
static NEXT_TCP_CLIENT_ID: AtomicUsize = AtomicUsize::new(10_000);

/// Maximum number of concurrent TCP client connections per server (issue #23).
const MAX_TCP_CLIENTS: usize = 100;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Runs the TCP endpoint logic, continuously handling connections based on the specified mode.
///
/// This function sets up a TCP server or client and manages incoming/outgoing
/// MAVLink traffic through the provided message bus. It automatically retries
/// connections in client mode and gracefully handles multiple client connections
/// in server mode.
///
/// # Arguments
///
/// * `id` - Unique identifier for this endpoint.
/// * `address` - The TCP address to bind to (server) or connect to (client), e.g., "0.0.0.0:5760" or "127.0.0.1:5761".
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
/// Returns a [`crate::error::RouterError`] if:
/// - Binding to the specified address in server mode fails.
/// - An unrecoverable error occurs during connection establishment in client mode.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    address: String,
    mode: crate::config::EndpointMode,
    bus_tx: broadcast::Sender<Arc<RoutedMessage>>,
    bus_rx: broadcast::Receiver<Arc<RoutedMessage>>,
    routing_table: Arc<RoutingTable>,
    route_update_tx: tokio::sync::mpsc::Sender<crate::routing::RouteUpdate>,
    dedup: ConcurrentDedup,
    filters: EndpointFilters,
    token: CancellationToken,
    stats: Arc<EndpointStats>,
) -> Result<()> {
    // With tokio::broadcast, new subscribers are created via bus_tx.subscribe()
    // rather than cloning bus_rx. Drop the receiver to avoid unused variable warnings.
    drop(bus_rx);

    let core = EndpointCore {
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        route_update_tx: route_update_tx.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
        update_routing: true, // TCP client mode updates routing table
        stats,
    };

    match mode {
        crate::config::EndpointMode::Server => {
            let listener = TcpListener::bind(&address)
                .await
                .map_err(|e| RouterError::network(&address, e))?;
            info!("TCP Server listening on {}", address);

            let mut join_set = JoinSet::new();

            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, addr)) => {
                                // Enforce connection limit (issue #23)
                                if join_set.len() >= MAX_TCP_CLIENTS {
                                    warn!("TCP Server {}: Max client limit ({}) reached, rejecting {}", address, MAX_TCP_CLIENTS, addr);
                                    drop(stream);
                                    continue;
                                }

                                if let Err(e) = stream.set_nodelay(true) {
                                    debug!("set_nodelay failed for {}: {}", addr, e);
                                }

                                // Generate unique endpoint ID using global counter (issue #6)
                                let client_id = NEXT_TCP_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
                                info!("Accepted TCP connection from {} (EndpointId: {})", addr, client_id);

                                // Create a unique core for this client with its own EndpointId
                                let core_client = EndpointCore {
                                    id: EndpointId(client_id),
                                    bus_tx: core.bus_tx.clone(),
                                    routing_table: core.routing_table.clone(),
                                    route_update_tx: core.route_update_tx.clone(),
                                    dedup: core.dedup.clone(),
                                    filters: core.filters.clone(),
                                    update_routing: true, // Required for targeted message routing
                                    stats: Arc::new(EndpointStats::new()),
                                };
                                let rx_client = core.bus_tx.subscribe();
                                let token_client = token.clone();

                                join_set.spawn(async move {
                                    let (read, write) = stream.into_split();
                                    let name = format!("TCP Client {}", addr);
                                    if let Err(e) = run_stream_loop(
                                        read,
                                        write,
                                        rx_client,
                                        core_client,
                                        token_client,
                                        name.clone(),
                                    )
                                    .await
                                    {
                                        warn!("{} stream loop ended with error: {}", name, e);
                                    }
                                });
                            }
                            Err(e) => error!("TCP Accept error: {}", e),
                        }
                    }
                    _ = join_set.join_next(), if !join_set.is_empty() => {}
                    _ = token.cancelled() => break,
                }
            }
            join_set.shutdown().await;
            Ok(())
        }
        crate::config::EndpointMode::Client => {
            info!("Connecting to TCP server at {}", address);
            let mut backoff =
                ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);

            loop {
                if token.is_cancelled() {
                    break;
                }

                match TcpStream::connect(&address).await {
                    Ok(stream) => {
                        if let Err(e) = stream.set_nodelay(true) {
                            debug!("set_nodelay failed for {}: {}", address, e);
                        }
                        info!("Connected to {}", address);
                        backoff.reset();
                        let (read, write) = stream.into_split();
                        let name = format!("TCP Client {}", address);
                        if let Err(e) = run_stream_loop(
                            read,
                            write,
                            bus_tx.subscribe(),
                            core.clone(),
                            token.clone(),
                            name.clone(),
                        )
                        .await
                        {
                            // A follow-up warn! about reconnecting is logged below;
                            // keep the error details at debug to avoid duplicate noise.
                            debug!("{} stream loop ended with error: {}", name, e);
                        }
                        warn!("Connection to {} lost, retrying...", address);
                    }
                    Err(e) => {
                        warn!("Failed to connect to {}: {}. Retrying...", address, e);
                    }
                }

                let wait = backoff.next_backoff();
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {},
                    _ = token.cancelled() => break,
                }
            }
            Ok(())
        }
    }
}
