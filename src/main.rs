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
mod mavlink_utils;
mod routing;
mod stats;

use crate::config::{Config, EndpointConfig};
use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::ExponentialBackoff;
use crate::error::Result;
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
                inner: signal(SignalKind::hangup())?,
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
        let mut handles = vec![];

        let routing_table = Arc::new(RwLock::new(RoutingTable::new()));

        let cancel_token = CancellationToken::new(); // Correct placement

        let dedup_period = config.general.dedup_period_ms.unwrap_or(0);
        let dedup = ConcurrentDedup::new(Duration::from_millis(dedup_period));

        // Only spawn dedup rotator if dedup is enabled (rotation_interval > 0)
        let dedup_rotation_interval = dedup.rotation_interval();
        if !dedup_rotation_interval.is_zero() {
            let dedup_rotator = dedup.clone();
            let dedup_token = cancel_token.child_token();
            handles.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(dedup_rotation_interval);
                loop {
                    tokio::select! {
                        _ = dedup_token.cancelled() => {
                            info!("Dedup Rotator shutting down.");
                            break;
                        }
                        _ = interval.tick() => {
                            dedup_rotator.rotate_buckets();
                        }
                    }
                }
            }));
        }

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

        // Stats Query Interface (Unix Socket)
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

        // Implicit TCP Server
        if let Some(port) = config.general.tcp_port {
            let name = format!("Implicit TCP Server :{}", port);
            let bus_tx = bus.sender();
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
                    let bus_rx = bus_rx.clone();
                    let rt = rt.clone();
                    let dd = dd.clone();
                    let filters = filters.clone();
                    let addr = addr.clone();
                    let m = crate::config::EndpointMode::Server;
                    let token = task_token.clone();
                    async move {
                        crate::endpoints::tcp::run(
                            id, addr, m, bus_tx, bus_rx, rt, dd, filters, token,
                        )
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
                        let bus_rx = bus_rx.clone();
                        let dir = dir.clone();
                        let token = task_token.clone();
                        async move { crate::endpoints::tlog::run(dir, bus_rx, token).await }
                    },
                )));
            }
        }

        for (i, endpoint_config) in config.endpoint.iter().enumerate() {
            let bus = bus.clone();
            let bus_tx = bus.sender();
            let routing_table = routing_table.clone();
            let dedup = dedup.clone();
            let task_token = cancel_token.child_token();

            match endpoint_config {
                EndpointConfig::Udp {
                    address,
                    mode,
                    filters,
                } => {
                    let name = format!("UDP Endpoint {} ({})", i, address);
                    let address = address.clone();
                    let mode = mode.clone();
                    let filters = filters.clone();
                    let cleanup_ttl = prune_ttl;

                    handles.push(tokio::spawn(supervise(
                        name,
                        task_token.clone(),
                        move || {
                            crate::endpoints::udp::run(
                                i,
                                address.clone(),
                                mode.clone(),
                                bus_tx.clone(),
                                bus.subscribe(),
                                routing_table.clone(),
                                dedup.clone(),
                                filters.clone(),
                                task_token.clone(),
                                cleanup_ttl,
                            )
                        },
                    )));
                }
                EndpointConfig::Tcp {
                    address,
                    mode,
                    filters,
                } => {
                    let name = format!("TCP Endpoint {} ({})", i, address);
                    let address = address.clone();
                    let mode = mode.clone();
                    let filters = filters.clone();

                    handles.push(tokio::spawn(supervise(
                        name,
                        task_token.clone(),
                        move || {
                            crate::endpoints::tcp::run(
                                i,
                                address.clone(),
                                mode.clone(),
                                bus_tx.clone(),
                                bus.subscribe(),
                                routing_table.clone(),
                                dedup.clone(),
                                filters.clone(),
                                task_token.clone(),
                            )
                        },
                    )));
                }
                EndpointConfig::Serial {
                    device,
                    baud,
                    filters,
                } => {
                    let name = format!("Serial Endpoint {} ({})", i, device);
                    let device = device.clone();
                    let baud = *baud;
                    let filters = filters.clone();

                    handles.push(tokio::spawn(supervise(
                        name,
                        task_token.clone(),
                        move || {
                            crate::endpoints::serial::run(
                                i,
                                device.clone(),
                                baud,
                                bus_tx.clone(),
                                bus.subscribe(),
                                routing_table.clone(),
                                dedup.clone(),
                                filters.clone(),
                                task_token.clone(),
                            )
                        },
                    )));
                }
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

        if !reload {
            break;
        }

        // Give tasks time to flush (e.g. tlog) and cleanup
        tokio::time::sleep(Duration::from_secs(1)).await;
        info!("Restarting system...");
    }

    info!("Shutdown complete.");
    Ok(())
}

async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30), 2.0);

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

        let wait = backoff.next_backoff();
        info!(
            "Supervisor: Waiting {:.1?} before restarting {}",
            wait, name
        );
        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} received cancellation signal during backoff. Exiting.", name);
                break;
            }
        }
    }
}
