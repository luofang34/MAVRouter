//! Serial endpoint for MAVLink communications.
//!
//! This module handles MAVLink traffic over serial ports, providing
//! reliable communication with connected flight controllers or other
//! serial MAVLink devices. It supports automatic reconnection if the
//! serial port connection is lost.

use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::{run_stream_loop, EndpointCore, EndpointStats};
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

/// Maximum time the kernel is given to hand us an open serial handle
/// before we fail out and let the supervisor retry. `open_native_async`
/// is synchronously blocking despite its name — on wedged USB adapters
/// or stuck tty drivers it can sit indefinitely, which would block
/// whichever tokio worker thread it landed on.
const OPEN_TIMEOUT: Duration = Duration::from_secs(3);

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
/// Returns a [`crate::error::RouterError`] if a critical error occurs that prevents further
/// operation, such as a permanent serial port configuration issue.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    id: usize,
    device: String,
    baud: u32,
    flow_control: tokio_serial::FlowControl,
    bus_tx: broadcast::Sender<Arc<RoutedMessage>>,
    bus_rx: broadcast::Receiver<Arc<RoutedMessage>>,
    routing_table: Arc<RoutingTable>,
    route_update_tx: tokio::sync::mpsc::Sender<crate::routing::RouteUpdate>,
    dedup: ConcurrentDedup,
    filters: EndpointFilters,
    token: CancellationToken,
    stats: Arc<EndpointStats>,
) -> Result<()> {
    let core = EndpointCore {
        id: EndpointId(id),
        bus_tx: bus_tx.clone(),
        routing_table: routing_table.clone(),
        route_update_tx: route_update_tx.clone(),
        dedup: dedup.clone(),
        filters: filters.clone(),
        update_routing: true,
        stats,
    };

    // No inner retry loop - supervisor handles retries with exponential backoff (issue #26)
    open_and_run(&device, baud, flow_control, bus_rx, core, token).await
}

/// Synchronously open the serial device. Must be called on a blocking
/// thread (via `tokio::task::spawn_blocking`) because
/// `open_native_async` — despite the name — blocks until the kernel
/// driver completes the open.
fn open_serial_port_blocking(
    device: &str,
    baud: u32,
    flow_control: tokio_serial::FlowControl,
) -> std::result::Result<tokio_serial::SerialStream, tokio_serial::Error> {
    tokio_serial::new(device, baud)
        .flow_control(flow_control)
        .open_native_async()
}

/// Race a serial-open future against a deadline. Factored out so the
/// timeout mapping can be unit-tested without a real serial device.
async fn apply_open_timeout<T, Fut>(device: &str, timeout: Duration, open_fut: Fut) -> Result<T>
where
    Fut: std::future::Future<Output = std::result::Result<T, tokio_serial::Error>>,
{
    match tokio::time::timeout(timeout, open_fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(RouterError::serial(device, e)),
        Err(_) => Err(RouterError::serial_timeout(device, timeout.as_secs())),
    }
}

#[allow(unused_mut)]
async fn open_and_run(
    device: &str,
    baud: u32,
    flow_control: tokio_serial::FlowControl,
    bus_rx: broadcast::Receiver<Arc<RoutedMessage>>,
    core: EndpointCore,
    token: CancellationToken,
) -> Result<()> {
    let device_owned = device.to_string();
    let open_fut = async move {
        match tokio::task::spawn_blocking(move || {
            open_serial_port_blocking(&device_owned, baud, flow_control)
        })
        .await
        {
            Ok(inner) => inner,
            Err(join_err) => Err(tokio_serial::Error::new(
                tokio_serial::ErrorKind::Io(io::ErrorKind::Other),
                format!("serial open task panicked: {}", join_err),
            )),
        }
    };
    let mut port = apply_open_timeout(device, OPEN_TIMEOUT, open_fut).await?;

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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::panic)]

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn apply_open_timeout_passes_through_success() {
        let fut = async { Ok::<u32, tokio_serial::Error>(42) };
        let r = apply_open_timeout("fake", Duration::from_secs(3), fut).await;
        assert_eq!(r.expect("success case should not error"), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn apply_open_timeout_wraps_inner_error() {
        let fut = async {
            Err::<(), _>(tokio_serial::Error::new(
                tokio_serial::ErrorKind::NoDevice,
                "no such device",
            ))
        };
        let r = apply_open_timeout("/dev/ttyNOPE", Duration::from_secs(3), fut).await;
        match r {
            Err(RouterError::Serial { device, source: _ }) => {
                assert_eq!(device, "/dev/ttyNOPE");
            }
            other => panic!("expected RouterError::Serial, got {:?}", other),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn apply_open_timeout_emits_serial_timeout_when_inner_blocks() {
        // Inner future never resolves within the timeout window. With
        // paused time tokio auto-advances to the nearest timer — the
        // 3s timeout fires in virtual time, so the test is cheap.
        let fut = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok::<(), tokio_serial::Error>(())
        };
        let r = apply_open_timeout("/dev/ttyWEDGED", Duration::from_secs(3), fut).await;
        match r {
            Err(RouterError::SerialTimeout {
                device,
                timeout_secs,
            }) => {
                assert_eq!(device, "/dev/ttyWEDGED");
                assert_eq!(timeout_secs, 3);
            }
            other => panic!("expected RouterError::SerialTimeout, got {:?}", other),
        }
    }
}
