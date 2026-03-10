#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod config;
mod endpoint_core;
mod error;
mod router;
mod endpoints {
    pub mod serial;
    pub mod tcp;
    pub mod tlog;
    pub mod udp;
}
mod dedup;
mod filter;
mod framing;
#[allow(dead_code)] // Router struct is only used via the library crate
mod high_level;
mod mavlink_utils;
mod routing;
mod stats;

use crate::config::Config;
use crate::endpoint_core::supervise;
use crate::error::{Result, RouterError};
use crate::high_level::spawn_all_tasks;
use crate::router::create_bus;
use crate::routing::RoutingTable;
use crate::stats::StatsHistory;
use clap::Parser;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(unix)]
use tokio::signal::unix::{signal, Signal, SignalKind};

#[cfg(unix)]
async fn run_stats_server(
    socket_path: String,
    routing_table: Arc<RwLock<RoutingTable>>,
    token: CancellationToken,
) -> crate::error::Result<()> {
    let path = Path::new(&socket_path);
    if path.exists() {
        tokio::fs::remove_file(path)
            .await
            .map_err(|e| RouterError::filesystem(&socket_path, e))?;
    }

    let listener = UnixListener::bind(path)
        .map_err(|e| RouterError::filesystem(&socket_path, e))?;
    info!("Stats Query Interface listening on {}", socket_path);

    let metadata = std::fs::metadata(path)
        .map_err(|e| RouterError::filesystem(&socket_path, e))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o660); // Restrict to owner/group read/write
    std::fs::set_permissions(path, permissions)
        .map_err(|e| RouterError::filesystem(&socket_path, e))?;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                info!("Stats Server shutting down.");
                if path.exists() {
                    let _ = tokio::fs::remove_file(path).await;
                }
                break;
            }
            accept_res = listener.accept() => {
                match accept_res {
                    Ok((mut stream, _addr)) => {
                        let rt = routing_table.clone();
                        tokio::spawn(async move {
                            let mut buf = [0u8; 1024];
                            let n = match stream.read(&mut buf).await {
                                Ok(n) if n > 0 => n,
                                _ => return,
                            };

                            let command = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                            let response = match command.as_str() {
                                "stats" => {
                                    let rt_guard = rt.read();
                                    let stats = rt_guard.stats();
                                    format!(
                                        r#"{{"total_systems":{},"total_routes":{},"total_endpoints":{},"timestamp":{}}}"#,
                                        stats.total_systems,
                                        stats.total_routes,
                                        stats.total_endpoints,
                                        stats.timestamp
                                    )
                                }
                                "help" => "Available commands: stats, help".to_string(),
                                _ => "Unknown command. Try 'help'".to_string(),
                            };

                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                    Err(e) => {
                        warn!("Stats Server accept error: {}", e);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Helper struct to handle SIGHUP on Unix and do nothing on Windows
struct ReloadSignal {
    #[cfg(unix)]
    inner: Signal,
}

impl ReloadSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: signal(SignalKind::hangup())
                    .map_err(|e| RouterError::Internal(format!("Failed to register SIGHUP: {}", e)))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) -> Option<()> {
        #[cfg(unix)]
        {
            self.inner.recv().await
        }
        #[cfg(not(unix))]
        {
            std::future::pending().await
        }
    }
}

/// Helper struct to handle SIGTERM on Unix and do nothing on Windows
struct TerminateSignal {
    #[cfg(unix)]
    inner: Signal,
}

impl TerminateSignal {
    fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: signal(SignalKind::terminate())
                    .map_err(|e| RouterError::Internal(format!("Failed to register SIGTERM: {}", e)))?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    async fn recv(&mut self) -> Option<()> {
        #[cfg(unix)]
        {
            self.inner.recv().await
        }
        #[cfg(not(unix))]
        {
            std::future::pending().await
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "mavrouter.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // AGPL-3.0 License Notice
    info!("mavrouter-rs v{} - AGPL-3.0", env!("CARGO_PKG_VERSION"));
    info!("Source code: {}", env!("CARGO_PKG_REPOSITORY"));
    info!("This program comes with ABSOLUTELY NO WARRANTY.");

    let args = Args::parse();

    info!("Starting mavrouter-rs with config: {}", args.config);

    // Initialize reload signal handler
    let mut sig_hup = ReloadSignal::new()?;

    // Initialize SIGTERM handler for graceful shutdown
    let mut sig_term = TerminateSignal::new()?;

    loop {
        let config = match Config::load(&args.config).await {
            Ok(c) => c,
            Err(e) => {
                error!("Error loading config: {:#}", e);
                return Err(e);
            }
        };

        info!(
            "Loaded configuration with {} endpoints",
            config.endpoint.len()
        );

        let bus = create_bus(config.general.bus_capacity);
        let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
        let cancel_token = CancellationToken::new();

        // Spawn all shared tasks (dedup, pruner, endpoints, tlog, implicit tcp)
        let mut handles = spawn_all_tasks(&config, &bus, &routing_table, &cancel_token);

        // Binary-only: Stats Reporting Task
        let sample_interval = config.general.stats_sample_interval_secs;
        let retention = config.general.stats_retention_secs;
        let log_interval = config.general.stats_log_interval_secs;

        if sample_interval > 0 && retention > 0 {
            let rt_stats = routing_table.clone();
            let stats_token = cancel_token.child_token();

            handles.push(tokio::spawn(async move {
                let mut history = StatsHistory::new(retention);
                let mut last_log_time = 0u64;

                loop {
                    tokio::select! {
                        _ = stats_token.cancelled() => {
                            info!("Stats Reporter shutting down.");
                            break;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(sample_interval)) => {
                            let rt = rt_stats.read();
                            let mut stats = rt.stats();
                            drop(rt); // Release lock quickly
                            // Ensure timestamp reflects sample time
                            stats.timestamp = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let current_timestamp = stats.timestamp;
                            history.push(stats.clone());

                            // Periodic logging
                            if current_timestamp.saturating_sub(last_log_time) >= log_interval {
                                // 1 minute aggregation
                                if let Some(min1) = history.aggregate(60) {
                                    info!(
                                        "Stats [1min] avg={:.1} routes, range=[{}-{}]",
                                        min1.avg_routes, min1.min_routes, min1.max_routes
                                    );
                                }

                                // 1 hour aggregation
                                if let Some(hour1) = history.aggregate(3600) {
                                    info!(
                                        "Stats [1hr] avg={:.1} routes, max={}",
                                        hour1.avg_routes, hour1.max_routes
                                    );
                                }

                                // Full retention aggregation
                                if let Some(all) = history.aggregate(retention) {
                                    info!(
                                        "Stats [{}h] avg={:.1} routes, samples={}, systems={}, endpoints={}",
                                        retention / 3600,
                                        all.avg_routes,
                                        all.sample_count,
                                        stats.total_systems,
                                        stats.total_endpoints
                                    );
                                }

                                last_log_time = current_timestamp;
                            }
                        }
                    }
                }
            }));
        }

        // Binary-only: Stats Query Interface (Unix Socket)
        #[cfg(unix)]
        if let Some(socket_path) = config.general.stats_socket_path.clone() {
            if !socket_path.is_empty() {
                let name = format!("Stats Socket {}", socket_path);
                let rt = routing_table.clone();
                let task_token = cancel_token.child_token();
                let path = socket_path.clone();

                handles.push(tokio::spawn(supervise(
                    name,
                    task_token.clone(),
                    move || {
                        let rt = rt.clone();
                        let token = task_token.clone();
                        let p = path.clone();
                        async move { run_stats_server(p, rt, token).await }
                    },
                )));
            }
        }

        if handles.is_empty() {
            info!("No endpoints configured. Exiting.");
            return Ok(());
        }

        let mut reload = false;
        let mut all_tasks = Box::pin(futures::future::join_all(handles));

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Ctrl+C received. Initiating graceful shutdown...");
                    break;
                }
                _ = sig_term.recv() => {
                    info!("SIGTERM received. Initiating graceful shutdown...");
                    break;
                }
                _ = sig_hup.recv() => {
                    info!("SIGHUP received. Checking configuration for reload...");
                    match Config::load(&args.config).await {
                        Ok(_) => {
                            info!("Configuration valid. Restarting...");
                            reload = true;
                            break;
                        }
                        Err(e) => {
                            error!("Configuration invalid: {}. Ignoring SIGHUP.", e);
                        }
                    }
                }
                _ = &mut all_tasks => {
                    info!("All supervised tasks completed/failed. Shutting down.");
                    break;
                }
            }
        }

        cancel_token.cancel();

        // Give tasks time to flush (e.g. tlog) and cleanup
        tokio::time::sleep(Duration::from_millis(1000)).await;

        if !reload {
            break;
        }

        info!("Restarting system...");
    }

    info!("Shutdown complete.");
    Ok(())
}
