#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]

mod config;
mod endpoint_core;
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
mod mavlink_utils;
mod routing;
mod stats;

use crate::config::{Config, EndpointConfig};
use crate::dedup::Dedup;
use crate::endpoint_core::ExponentialBackoff;
use crate::router::create_bus;
use crate::routing::{RoutingStats, RoutingTable};
use crate::stats::StatsHistory;
use anyhow::Result;
use clap::Parser;
use parking_lot::{Mutex, RwLock};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
async fn run_stats_server(
    socket_path: String,
    routing_table: Arc<RwLock<RoutingTable>>,
    token: CancellationToken,
) -> Result<()> {
    let path = Path::new(&socket_path);
    if path.exists() {
        tokio::fs::remove_file(path).await?;
    }

    let listener = UnixListener::bind(path)?;
    info!("Stats Query Interface listening on {}", socket_path);

    let metadata = std::fs::metadata(path)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o660); // Restrict to owner/group read/write
    std::fs::set_permissions(path, permissions)?;

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

    let args = Args::parse();

    info!("Starting mavrouter-rs with config: {}", args.config);

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
    let mut handles = vec![];

    let routing_table = Arc::new(RwLock::new(RoutingTable::new()));

    let dedup_period = config.general.dedup_period_ms.unwrap_or(0);
    let dedup = Arc::new(Mutex::new(Dedup::new(Duration::from_millis(dedup_period))));

    let cancel_token = CancellationToken::new();

    // Pruning Task
    let rt_prune = routing_table.clone();
    let prune_token = cancel_token.child_token();
    let prune_interval = config.general.routing_table_prune_interval_secs;
    let prune_ttl = config.general.routing_table_ttl_secs;
    handles.push(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = prune_token.cancelled() => {
                    info!("RoutingTable Pruner shutting down.");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(prune_interval)) => {
                    let mut rt = rt_prune.write();
                    rt.prune(Duration::from_secs(prune_ttl));
                }
            }
        }
    }));

    // Stats Reporting Task
    let rt_stats = routing_table.clone();
    let stats_token = cancel_token.child_token();

    let sample_interval = config.general.stats_sample_interval_secs;
    let retention = config.general.stats_retention_secs;
    let log_interval = config.general.stats_log_interval_secs;

    if sample_interval > 0 && retention > 0 {
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
                        // Ensure timestamp reflects sample time, even if already set in routing.rs
                        stats.timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let current_timestamp = stats.timestamp; // Capture timestamp before move
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

    // Stats Query Interface (Unix Socket)
    #[cfg(unix)]
    if let Some(socket_path) = config.general.stats_socket_path {
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

    // Implicit TCP Server
    if let Some(port) = config.general.tcp_port {
        let name = format!("Implicit TCP Server :{}", port);
        let bus_tx = bus.clone();
        let bus_rx = bus.subscribe();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let id = config.endpoint.len();
        let filters = crate::filter::EndpointFilters::default();
        let addr = format!("0.0.0.0:{}", port);
        let task_token = cancel_token.child_token();

        handles.push(tokio::spawn(supervise(
            name,
            task_token.clone(),
            move || {
                let bus_tx = bus_tx.clone();
                let bus_rx = bus_rx.resubscribe();
                let rt = rt.clone();
                let dd = dd.clone();
                let filters = filters.clone();
                let addr = addr.clone();
                let m = crate::config::EndpointMode::Server;
                let token = task_token.clone();
                async move {
                    crate::endpoints::tcp::run(id, addr, m, bus_tx, bus_rx, rt, dd, filters, token)
                        .await
                }
            },
        )));
    }

    // Logging
    if let Some(log_dir) = &config.general.log {
        if config.general.log_telemetry {
            let name = format!("TLog Logger {}", log_dir);
            let bus_rx = bus.subscribe();
            let dir = log_dir.clone();
            let task_token = cancel_token.child_token();

            handles.push(tokio::spawn(supervise(
                name,
                task_token.clone(),
                move || {
                    let bus_rx = bus_rx.resubscribe();
                    let dir = dir.clone();
                    let token = task_token.clone();
                    async move { crate::endpoints::tlog::run(dir, bus_rx, token).await }
                },
            )));
        }
    }

    for (i, endpoint_config) in config.endpoint.iter().enumerate() {
        let bus_tx = bus.clone();
        let bus_rx = bus.subscribe();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let task_token = cancel_token.child_token();

        match endpoint_config {
            EndpointConfig::Udp {
                address,
                mode,
                filters,
            } => {
                let name = format!("UDP Endpoint {} ({})", i, address);
                let addr = address.clone();
                let m = mode.clone();
                let f = filters.clone();

                handles.push(tokio::spawn(supervise(
                    name,
                    task_token.clone(),
                    move || {
                        let bus_tx = bus_tx.clone();
                        let bus_rx = bus_rx.resubscribe();
                        let rt = rt.clone();
                        let dd = dd.clone();
                        let f = f.clone();
                        let addr = addr.clone();
                        let m = m.clone();
                        let token = task_token.clone();
                        async move {
                            crate::endpoints::udp::run(i, addr, m, bus_tx, bus_rx, rt, dd, f, token)
                                .await
                        }
                    },
                )));
            }
            EndpointConfig::Tcp {
                address,
                mode,
                filters,
            } => {
                let name = format!("TCP Endpoint {} ({})", i, address);
                let addr = address.clone();
                let m = mode.clone();
                let f = filters.clone();

                handles.push(tokio::spawn(supervise(
                    name,
                    task_token.clone(),
                    move || {
                        let bus_tx = bus_tx.clone();
                        let bus_rx = bus_rx.resubscribe();
                        let rt = rt.clone();
                        let dd = dd.clone();
                        let f = f.clone();
                        let addr = addr.clone();
                        let m = m.clone();
                        let token = task_token.clone();
                        async move {
                            crate::endpoints::tcp::run(i, addr, m, bus_tx, bus_rx, rt, dd, f, token)
                                .await
                        }
                    },
                )));
            }
            EndpointConfig::Serial {
                device,
                baud,
                filters,
            } => {
                let name = format!("Serial Endpoint {} ({})", i, device);
                let dev = device.clone();
                let b = *baud;
                let f = filters.clone();

                handles.push(tokio::spawn(supervise(
                    name,
                    task_token.clone(),
                    move || {
                        let bus_tx = bus_tx.clone();
                        let bus_rx = bus_rx.resubscribe();
                        let rt = rt.clone();
                        let dd = dd.clone();
                        let f = f.clone();
                        let dev = dev.clone();
                        let token = task_token.clone();
                        async move {
                            crate::endpoints::serial::run(
                                i, dev, b, bus_tx, bus_rx, rt, dd, f, token,
                            )
                            .await
                        }
                    },
                )));
            }
        }
    }

    if handles.is_empty() {
        info!("No endpoints configured. Exiting.");
        return Ok(());
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received. Initiating graceful shutdown...");
        }
        _ = futures::future::join_all(handles) => {
            info!("All supervised tasks completed/failed. Shutting down.");
        }
    }
    cancel_token.cancel();

    // Give tasks time to flush (e.g. tlog)
    tokio::time::sleep(Duration::from_secs(2)).await;
    info!("Shutdown complete.");

    Ok(())
}

async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = ExponentialBackoff::new(
        Duration::from_secs(1),
        Duration::from_secs(30),
        2.0,
    );

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} received cancellation signal. Exiting.", name);
                break;
            }
            _ = async {
                let start_time = std::time::Instant::now();
                let result = task_factory().await;
                
                // If task ran for more than 60 seconds, reset backoff
                if start_time.elapsed() > Duration::from_secs(60) {
                    backoff.reset();
                }

                match result {
                    Ok(_) => {
                        warn!("Supervisor: Task {} finished cleanly (unexpected). Restarting...", name);
                    }
                    Err(e) => {
                        error!("Supervisor: Task {} failed: {:#}. Restarting...", name, e);
                    }
                }
            } => {}
        }
        
        let wait = backoff.next();
        info!("Supervisor: Waiting {:.1?} before restarting {}", wait, name);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} received cancellation signal during backoff. Exiting.", name);
                break;
            }
        }
    }
}
