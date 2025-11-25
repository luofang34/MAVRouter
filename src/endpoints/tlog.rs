//! TLOG endpoint for logging MAVLink messages to disk.
//!
//! This module provides functionality to capture all MAVLink messages
//! flowing through the router's message bus and save them to a
//! TLOG (Telemetry Log) file. Each TLOG file is timestamped, and
//! messages are prepended with their arrival timestamp.

use crate::error::{Result, RouterError};
use crate::router::RoutedMessage;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Runs the TLOG logging endpoint, continuously saving MAVLink messages to a file.
///
/// This function listens for `RoutedMessage`s on the message bus, formats them
/// with a timestamp, and writes them to a TLOG file in the specified directory.
/// A new TLOG file is created for each run, named with a timestamp.
///
/// # Arguments
///
/// * `logs_dir` - The directory where TLOG files should be saved.
/// * `bus_rx` - Receiver half of the message bus, subscribed to all `RoutedMessage`s.
/// * `cancel_token` - `CancellationToken` to signal graceful shutdown.
///
/// # Returns
///
/// A `Result` indicating success or failure. The function will run indefinitely
/// until the `CancellationToken` is cancelled or a critical error occurs.
///
/// # Errors
///
/// Returns an `anyhow::Error` if:
/// - The `logs_dir` cannot be created.
/// - The TLOG file cannot be created.
/// - An error occurs during writing to the TLOG file.
pub async fn run(
    logs_dir: String,
    mut bus_rx: broadcast::Receiver<RoutedMessage>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let dir = Path::new(&logs_dir);
    if !dir.exists() {
        fs::create_dir_all(dir)
            .await
            .map_err(|e| RouterError::filesystem(&logs_dir, e))?;
    }

    let now = SystemTime::now();
    let since_the_epoch_us = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0))
        .as_micros() as u64;
    let filename = format!("flight_{}.tlog", since_the_epoch_us);
    let path = dir.join(filename);

    info!("TLog Logger: Logging to {:?}", path);

    let file = File::create(&path)
        .await
        .map_err(|e| RouterError::filesystem(path.display().to_string(), e))?;
    let mut writer = tokio::io::BufWriter::new(file);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("TLog Logger: Shutdown signal received.");
                break;
            }
            res = bus_rx.recv() => {
                                    match res {
                                        Ok(msg) => {
                                            // Use timestamp from RoutedMessage
                                            let timestamp_us = msg.timestamp_us;
                                            let ts_bytes = timestamp_us.to_be_bytes();

                                            if let Err(e) = writer.write_all(&ts_bytes).await {
                                                error!("TLog write error: {}", e);
                                                break;
                                            }
                                            // Use pre-serialized bytes
                                            if let Err(e) = writer.write_all(&msg.serialized_bytes).await {
                                                error!("TLog write error: {}", e);
                                                break;
                                            }
                                        }
                                        Err(broadcast::error::RecvError::Lagged(n)) => {
                                            warn!("TLog Logger lagged: missed {} messages", n);
                                        }
                                        Err(broadcast::error::RecvError::Closed) => break,
                                    }            }
        }
    }

    // Ensure all buffered data is written before exiting
    if let Err(e) = writer.flush().await {
        error!("TLog flush error: {}", e);
    }
    info!("TLog Logger finished flushing and closing.");
    Ok(())
}
