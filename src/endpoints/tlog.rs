use crate::router::RoutedMessage;
use anyhow::{Context, Result};
use mavlink::MavlinkVersion;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

pub async fn run(
    logs_dir: String,
    mut bus_rx: broadcast::Receiver<RoutedMessage>,
    cancel_token: CancellationToken,
) -> Result<()> {
    let dir = Path::new(&logs_dir);
    if !dir.exists() {
        fs::create_dir_all(dir)
            .await
            .context("Failed to create logs directory")?;
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
        .context("Failed to create log file")?;
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
                        let mut buf = Vec::new();
                        let res = match msg.version {
                             MavlinkVersion::V2 => mavlink::write_v2_msg(&mut buf, msg.header, &msg.message),
                             MavlinkVersion::V1 => mavlink::write_v1_msg(&mut buf, msg.header, &msg.message),
                        };

                        if res.is_ok() {
                            let timestamp_us = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or(std::time::Duration::from_secs(0))
                                .as_micros() as u64;
                            let ts_bytes = timestamp_us.to_be_bytes();

                            if let Err(e) = writer.write_all(&ts_bytes).await {
                                error!("TLog write error: {}", e);
                                break;
                            }
                            if let Err(e) = writer.write_all(&buf).await {
                                error!("TLog write error: {}", e);
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("TLog Logger lagged: missed {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
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
