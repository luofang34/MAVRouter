//! TLOG endpoint for logging MAVLink messages to disk.
//!
//! This module provides functionality to capture all MAVLink messages
//! flowing through the router's message bus and save them to a
//! TLOG (Telemetry Log) file. Each TLOG file is timestamped, and
//! messages are prepended with their arrival timestamp.

use crate::error::{Result, RouterError};
use crate::router::RoutedMessage;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Interval for periodic flushing of TLOG buffer
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

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
/// * `bus_lagged` - Shared counter bumped by `n` whenever the broadcast
///   receiver reports `Lagged(n)`. Owned by the caller so stats
///   aggregation can observe drops without TLog needing its own stats
///   surface.
/// * `cancel_token` - `CancellationToken` to signal graceful shutdown.
///
/// # Returns
///
/// A `Result` indicating success or failure. The function will run indefinitely
/// until the `CancellationToken` is cancelled or a critical error occurs.
///
/// # Errors
///
/// Returns a [`crate::error::RouterError`] if:
/// - The `logs_dir` cannot be created.
/// - The TLOG file cannot be created.
/// - An error occurs during writing to the TLOG file.
pub async fn run(
    logs_dir: String,
    mut bus_rx: broadcast::Receiver<Arc<RoutedMessage>>,
    bus_lagged: Arc<AtomicU64>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let dir = Path::new(&logs_dir);
    if !dir.exists() {
        fs::create_dir_all(dir)
            .await
            .map_err(|e| RouterError::filesystem(&logs_dir, e))?;
    }

    let now = SystemTime::now();
    let since_the_epoch = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::from_secs(0))
        .as_micros();
    // Truncate to u64 — timestamp wraps after ~584,942 years
    #[allow(clippy::cast_possible_truncation)]
    let since_the_epoch_us = since_the_epoch as u64;
    let filename = format!("flight_{}.tlog", since_the_epoch_us);
    let path = dir.join(filename);

    info!("TLog Logger: Logging to {:?}", path);

    let path_str = path.display().to_string();

    let file = File::create(&path)
        .await
        .map_err(|e| RouterError::filesystem(&path_str, e))?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut flush_interval = tokio::time::interval(FLUSH_INTERVAL);
    // Don't count the initial tick
    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("TLog Logger: Shutdown signal received.");
                break;
            }
            _ = flush_interval.tick() => {
                // Periodic flush to ensure data is written to disk
                if let Err(e) = writer.flush().await {
                    error!("TLog periodic flush error: {}", e);
                    return Err(RouterError::filesystem(&path_str, e));
                }
            }
            res = bus_rx.recv() => {
                match res {
                    Ok(msg) => {
                        // Use timestamp from RoutedMessage
                        let timestamp_us = msg.timestamp_us;
                        let ts_bytes = timestamp_us.to_be_bytes();

                        if let Err(e) = writer.write_all(&ts_bytes).await {
                            error!("TLog write error: {}", e);
                            return Err(RouterError::filesystem(&path_str, e));
                        }
                        // Use pre-serialized bytes
                        if let Err(e) = writer.write_all(&msg.serialized_bytes).await {
                            error!("TLog write error: {}", e);
                            return Err(RouterError::filesystem(&path_str, e));
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Dropping TLog-bound messages is a correctness
                        // signal, not noise — count it and emit at
                        // error! so the stats pipeline and operators
                        // both see it (matches the convention in
                        // endpoint_core::stream_loop).
                        bus_lagged.fetch_add(n, Ordering::Relaxed);
                        error!(
                            "TLog Logger bus receiver lagged: dropped {} messages (bus_lagged now {})",
                            n,
                            bus_lagged.load(Ordering::Relaxed)
                        );
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        }
    }

    // Ensure all buffered data is written before exiting
    if let Err(e) = writer.flush().await {
        error!("TLog flush error: {}", e);
    }
    info!("TLog Logger finished flushing and closing.");
    Ok(())
}

#[cfg(test)]
mod tests;
