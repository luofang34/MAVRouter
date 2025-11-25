//! Serial endpoint for MAVLink communications.
//!
//! This module handles MAVLink traffic over serial ports, providing
//! reliable communication with connected flight controllers or other
//! serial MAVLink devices. It supports automatic reconnection if the
//! serial port connection is lost.

use crate::dedup::Dedup;
use crate::endpoint_core::{run_stream_loop, EndpointCore};
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Runs the serial endpoint logic, continuously attempting to open and
/// communicate over the specified serial device.
///
/// This function manages the lifecycle of a serial connection:
/// - It attempts to open the serial port with the given baud rate.
/// - If successful, it enters a loop to process incoming and outgoing MAVLink messages.
/// - If the port cannot be opened or the connection is lost, it retries after a delay.
/// - It gracefully shuts down when the `CancellationToken` is cancelled.
///
/// # Arguments
///
/// * `id` - Unique identifier for this endpoint.
/// * `device` - The path to the serial device (e.g., "/dev/ttyACM0" on Linux, "COM1" on Windows).
/// * `baud` - The baud rate for the serial connection (e.g., 115200).
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
/// until the `CancellationToken` is cancelled or a critical, unrecoverable error occurs.
///
/// # Errors
///
/// Returns an `anyhow::Error` if a critical error occurs that prevents further operation,
/// such as a permanent serial port configuration issue.
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
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
    };

    loop {
        match open_and_run(
            &device,
            baud,
            bus_rx.resubscribe(),
            core.clone(),
            token.clone(),
        )
        .await
        {
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

/// Opens the specified serial port and runs the stream processing loop.
///
/// This is a helper function that attempts to open a serial port and
/// then delegates to `run_stream_loop` to handle the actual MAVLink
/// message processing.
///
/// # Arguments
///
/// * `device` - The path to the serial device.
/// * `baud` - The baud rate for the serial connection.
/// * `bus_rx` - Receiver half of the message bus for outgoing messages.
/// * `core` - The `EndpointCore` instance containing shared resources and logic.
/// * `token` - `CancellationToken` to signal graceful shutdown.
///
/// # Returns
///
/// A `Result` indicating success or failure to open the port or if `run_stream_loop`
/// encounters an error.
///
/// # Errors
///
/// Returns an `anyhow::Error` if the serial port cannot be opened or configured.
#[allow(unused_mut)]
async fn open_and_run(
    device: &str,
    baud: u32,
    bus_rx: broadcast::Receiver<RoutedMessage>,
    core: EndpointCore,
    token: CancellationToken,
) -> Result<()> {
    let mut port = tokio_serial::new(device, baud)
        .open_native_async()
        .map_err(|e| RouterError::serial(device, e))?;

    #[cfg(unix)]
    if let Err(e) = port.set_exclusive(false) {
        warn!("Failed to set exclusive mode on {}: {}", device, e);
    }

    info!("Serial endpoint opened at {} baud", baud);

    let (read_stream, write_stream) = tokio::io::split(port);

    run_stream_loop(
        read_stream,
        write_stream,
        bus_rx,
        core,
        token,
        device.to_string(),
    )
    .await
}
