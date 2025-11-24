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

    // Note: TCP Server implementation logic is slightly different from Generic Stream because
    // it broadcasts to ALL clients. The `EndpointCore` logic is per-client (1-to-1).
    // However, `handle_connection` is 1-to-1.
    // The SERVER Sender task (broadcasting to clients) must still exist if we want centralized sending?
    // Currently `handle_connection` does both reading and writing for that client.
    // If we use `run_stream_loop` for `handle_connection`, it works:
    // It reads from client -> Bus.
    // It reads from Bus -> Client.
    // This effectively makes every client an "Endpoint instance" sharing ID?
    // Yes. This is what my previous implementation did.
    // So `run` just accepts and spawns `run_stream_loop`.
    
    // Wait, previous impl had a `sender_task` in `run` that wrote to `clients` map?
    // Ah, yes. `tcp.rs` had a CENTRAL sender task.
    // Why? To avoid N bus subscriptions?
    // Broadcast channel handles N subscriptions efficiently.
    // If we use `run_stream_loop` per client, each client subscribes to broadcast.
    // This is simpler and likely fine for < 100 clients.
    // It simplifies logic massively.
    // Does it work? Yes. `broadcast` channel is designed for this.
    
    // So I will switch TCP Server to spawn `run_stream_loop` per client.
    
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
