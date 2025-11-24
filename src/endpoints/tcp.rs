use anyhow::{Result, Context};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use crate::router::RoutedMessage;
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use crate::routing::RoutingTable;
use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::endpoint_core::{EndpointCore, run_stream_loop};
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    address: String,
    mode: crate::config::EndpointMode,
    bus_tx: broadcast::Sender<RoutedMessage>,
    bus_rx: broadcast::Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: Arc<Mutex<Dedup>>,
    filters: EndpointFilters,
    token: CancellationToken,
) -> Result<()> {
    
    let core = EndpointCore {
        id,
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
    };

    match mode {
        crate::config::EndpointMode::Server => {
            let listener = TcpListener::bind(&address).await
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
            loop {
                if token.is_cancelled() { break; }
                
                match TcpStream::connect(&address).await {
                    Ok(stream) => {
                        info!("Connected to {}", address);
                        let (read, write) = tokio::io::split(stream);
                        let name = format!("TCP Client {}", address);
                        let _ = run_stream_loop(read, write, bus_rx.resubscribe(), core.clone(), token.clone(), name).await;
                        warn!("Connection to {} lost, retrying in 1s...", address);
                    }
                    Err(e) => {
                        error!("Failed to connect to {}: {}. Retrying in 5s...", address, e);
                    }
                }
                
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {},
                    _ = token.cancelled() => break,
                }
            }
            Ok(())
        }
    }
}
