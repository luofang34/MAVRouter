use crate::dedup::Dedup;
use crate::filter::EndpointFilters;
use crate::framing::StreamParser;
use crate::router::RoutedMessage;
use crate::routing::RoutingTable;
use anyhow::{Context, Result};
use mavlink::{MavlinkVersion, Message};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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
    loop {
        // Pass token to open_and_run?
        // Yes, so it can break its loop.
        match open_and_run(
            id,
            &device,
            baud,
            bus_tx.clone(),
            bus_rx.resubscribe(),
            routing_table.clone(),
            dedup.clone(),
            filters.clone(),
            token.clone(),
        )
        .await
        {
            Ok(_) => {
                // If open_and_run returns Ok, it might be EOF or Cancelled.
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

        // Check token before sleeping
        if token.is_cancelled() {
            break;
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            _ = token.cancelled() => { break; }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn open_and_run(
    id: usize,
    device: &str,
    baud: u32,
    bus_tx: broadcast::Sender<RoutedMessage>,
    mut bus_rx: broadcast::Receiver<RoutedMessage>,
    routing_table: Arc<RwLock<RoutingTable>>,
    dedup: Arc<Mutex<Dedup>>,
    filters: EndpointFilters,
    token: CancellationToken,
) -> Result<()> {
    let mut port = tokio_serial::new(device, baud)
        .open_native_async()
        .with_context(|| format!("Failed to open serial port {}", device))?;

    #[cfg(unix)]
    port.set_exclusive(false).ok();

    info!("Serial endpoint {} opened at {} baud", device, baud);

    let (mut read_stream, mut write_stream) = tokio::io::split(port);

    let filters_tx = filters.clone();

    // Writer Task
    let writer_loop = async move {
        loop {
            match bus_rx.recv().await {
                Ok(msg) => {
                    if msg.source_id == id {
                        continue;
                    }

                    if !filters_tx.check_outgoing(&msg.header, msg.message.message_id()) {
                        continue;
                    }

                    let mut buf = Vec::new();
                    if let Err(e) = match msg.version {
                        MavlinkVersion::V2 => {
                            mavlink::write_v2_msg(&mut buf, msg.header, &msg.message)
                        }
                        MavlinkVersion::V1 => {
                            mavlink::write_v1_msg(&mut buf, msg.header, &msg.message)
                        }
                    } {
                        warn!("Serial Serialize Error: {}", e);
                        continue;
                    }

                    if let Err(e) = write_stream.write_all(&buf).await {
                        error!("Serial write error: {}", e);
                        // If write fails, break loop to restart connection
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Serial Sender lagged: missed {} messages", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    // Reader Loop
    let reader_loop = async move {
        let mut parser = StreamParser::new();
        let mut buf = [0u8; 4096];

        loop {
            match read_stream.read(&mut buf).await {
                Ok(0) => {
                    warn!("Serial port {} EOF", device);
                    break Ok(());
                }
                Ok(n) => {
                    parser.push(&buf[..n]);

                    while let Some(frame) = parser.next() {
                        let mut temp_buf = Vec::new();
                        if let Err(e) = match frame.version {
                            MavlinkVersion::V2 => {
                                mavlink::write_v2_msg(&mut temp_buf, frame.header, &frame.message)
                            }
                            MavlinkVersion::V1 => {
                                mavlink::write_v1_msg(&mut temp_buf, frame.header, &frame.message)
                            }
                        } {
                            warn!("Serial Inbound Serialize Error: {}", e);
                            continue;
                        }

                        let is_dup = {
                            #[allow(clippy::expect_used)]
                            let mut dd = dedup.lock();
                            dd.is_duplicate(&temp_buf)
                        };

                        if !is_dup
                            && filters.check_incoming(&frame.header, frame.message.message_id())
                        {
                            {
                                #[allow(clippy::expect_used)]
                                let mut rt = routing_table.write();
                                rt.update(id, frame.header.system_id, frame.header.component_id);
                            }
                            if let Err(e) = bus_tx.send(RoutedMessage {
                                source_id: id,
                                header: frame.header,
                                message: frame.message,
                                version: frame.version,
                            }) {
                                debug!("Serial Bus Send Error: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Serial read error: {}", e);
                    break Err(anyhow::Error::new(e));
                }
            }
        }
    };

    tokio::select! {
        _ = writer_loop => Ok(()),
        res = reader_loop => res,
        _ = token.cancelled() => Ok(()),
    }
}
