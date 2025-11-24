use anyhow::{Result, Context};
use tokio_serial::SerialPortBuilderExt;
use tokio::sync::broadcast;
use tracing::{info, warn, error};
use crate::router::RoutedMessage;
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use crate::routing::RoutingTable;
use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::endpoint_core::{EndpointCore, run_stream_loop};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    device: String,
    baud: u32,
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

    loop {
        match open_and_run(&device, baud, bus_rx.resubscribe(), core.clone(), token.clone()).await {
            Ok(_) => {
                if token.is_cancelled() {
                    info!("Serial port {} loop stopped (cancelled).", device);
                    break;
                }
                warn!("Serial port {} loop ended cleanly, retrying...", device);
            }
            Err(e) => {
                if token.is_cancelled() {
                    break;
                }
                error!("Serial port {} error: {:#}. Retrying in 1s...", device, e);
            }
        }
        
        if token.is_cancelled() { break; }
        
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            _ = token.cancelled() => { break; }
        }
    }
    Ok(())
}

async fn open_and_run(
    device: &str,
    baud: u32,
    bus_rx: broadcast::Receiver<RoutedMessage>,
    core: EndpointCore,
    token: CancellationToken,
) -> Result<()> {
    let mut port = tokio_serial::new(device, baud)
        .open_native_async()
        .with_context(|| format!("Failed to open serial port {}", device))?;

    #[cfg(unix)]
    port.set_exclusive(false).ok(); 

    info!("Serial endpoint opened at {} baud", baud);
    
    let (read_stream, write_stream) = tokio::io::split(port);

    run_stream_loop(read_stream, write_stream, bus_rx, core, token, device.to_string()).await
}
