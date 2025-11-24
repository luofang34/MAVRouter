//! TCP endpoint for MAVLink connections.
//!
//! This module provides functionality to establish and manage TCP connections
//! for MAVLink communication, supporting both server and client modes.
//!
//! In **server mode**, it listens for incoming TCP connections and handles
//! each client in a separate task.
//! In **client mode**, it attempts to connect to a remote TCP server and
//! automatically retries connection if lost.

use crate::dedup::Dedup;
use crate::endpoint_core::{run_stream_loop, EndpointCore};
use crate::filter::EndpointFilters;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

// ... (impl ExponentialBackoff ...)

// ... (pub async fn run ...)
) -> Result<()> {
    let core = EndpointCore {
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
    };
// ...
    match mode {
        crate::config::EndpointMode::Server => {
            let listener = TcpListener::bind(&address)
                .await
                .with_context(|| format!("Failed to bind TCP listener to {}", address))?;
            info!("TCP Server listening on {}", address);

            let mut join_set = JoinSet::new();

            loop {
                tokio::select! {
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, addr)) => {
                                info!("Accepted TCP connection from {}", addr);
                                let core_client = core.clone();
                                let rx_client = bus_rx.resubscribe();
                                let token_client = token.clone();

                                join_set.spawn(async move {
                                    let (read, write) = tokio::io::split(stream);
                                    let name = format!("TCP Client {}", addr);
                                    let _ = run_stream_loop(read, write, rx_client, core_client, token_client, name).await;
                                });
                            }
                            Err(e) => error!("TCP Accept error: {}", e),
                        }
                    }
                    _ = join_set.join_next(), if !join_set.is_empty() => {}
                    _ = token.cancelled() => break,
                }
            }
            Ok(())
        }
        crate::config::EndpointMode::Client => {
            info!("Connecting to TCP server at {}", address);
            let mut backoff = ExponentialBackoff::new(
                Duration::from_secs(1),
                Duration::from_secs(60),
                2.0,
            );

            loop {
                if token.is_cancelled() {
                    break;
                }

                match TcpStream::connect(&address).await {
                    Ok(stream) => {
                        info!("Connected to {}", address);
                        backoff.reset();
                        let (read, write) = tokio::io::split(stream);
                        let name = format!("TCP Client {}", address);
                        let _ = run_stream_loop(
                            read,
                            write,
                            bus_rx.resubscribe(),
                            core.clone(),
                            token.clone(),
                            name,
                        )
                        .await;
                        warn!("Connection to {} lost, retrying...", address);
                    }
                    Err(e) => {
                        warn!("Failed to connect to {}: {}. Retrying...", address, e);
                    }
                }

                let wait = backoff.next();
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {},
                    _ = token.cancelled() => break,
                }
            }
            Ok(())
        }
    }
}

// handle_connection removed as it is replaced by run_stream_loop
