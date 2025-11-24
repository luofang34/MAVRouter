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
pub mod lock_helpers;
mod mavlink_utils;
mod routing;

use crate::config::{Config, EndpointConfig};
use crate::dedup::Dedup;
use crate::router::create_bus;
use crate::routing::{RoutingStats, RoutingTable};
use anyhow::Result;
use clap::Parser;
use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Simple Stats buffer history
struct StatsHistory {
    samples: VecDeque<RoutingStats>, // Recent N seconds samples
    max_age_secs: u64,               // Max retention time
}

impl StatsHistory {
    fn new(max_age_secs: u64) -> Self {
        let capacity = max_age_secs as usize;
        Self {
            samples: VecDeque::with_capacity(capacity),
            max_age_secs,
        }
    }

    /// Add sample and clean up old data
    fn push(&mut self, stats: RoutingStats) {
        self.samples.push_back(stats);

        if let Some(latest) = self.samples.back() {
            // Clean up old data exceeding max_age
            let cutoff = latest.timestamp.saturating_sub(self.max_age_secs);
            while let Some(oldest) = self.samples.front() {
                if oldest.timestamp < cutoff {
                    self.samples.pop_front();
                } else {
                    break;
                }
            }
        }
    }

    /// Calculate aggregated statistics for a specified time window
    fn aggregate(&self, window_secs: u64) -> Option<AggregatedStats> {
        if self.samples.is_empty() {
            return None;
        }

        let latest = self.samples.back()?;
        let cutoff = latest.timestamp.saturating_sub(window_secs);

        let window: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.timestamp >= cutoff)
            .collect();

        if window.is_empty() {
            return None;
        }

        let sum_routes: usize = window.iter().map(|s| s.total_routes).sum();
        let avg_routes = sum_routes as f64 / window.len() as f64;

        Some(AggregatedStats {
            avg_routes,
            max_routes: window.iter().map(|s| s.total_routes).max()?,
            min_routes: window.iter().map(|s| s.total_routes).min()?,
            sample_count: window.len(),
        })
    }
}

/// Simplified aggregated stats
#[derive(Debug)]
struct AggregatedStats {
    avg_routes: f64,
    max_routes: usize,
    min_routes: usize,
    sample_count: usize,
}

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

    // Set permissions to allow anyone to query stats (if needed)
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
                                    let rt_guard = lock_read!(rt);
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
                    let mut rt = lock_write!(rt_prune);
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
                        let rt = lock_read!(rt_stats);
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
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} received cancellation signal. Exiting.", name);
                break;
            }
            result = task_factory() => {
                match result {
                    Ok(_) => {
                        warn!("Supervisor: Task {} finished cleanly (unexpected). Restarting in 1s...", name);
                    }
                    Err(e) => {
                        error!("Supervisor: Task {} failed: {:#}. Restarting in 1s...", name, e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // Allow expect() in tests for descriptive failure messages
mod tests {
    use super::*;
    use crate::routing::RoutingStats;

    fn dummy_stats(
        routes: usize,
        systems: usize,
        endpoints: usize,
        timestamp: u64,
    ) -> RoutingStats {
        RoutingStats {
            total_routes: routes,
            total_systems: systems,
            total_endpoints: endpoints,
            timestamp,
        }
    }

    #[test]
    fn test_stats_history_retention() {
        let mut history = StatsHistory::new(60); // 1 minute retention

        // Add 30 samples, 1 second apart
        for i in 0..30 {
            history.push(dummy_stats(i, i, i, i as u64));
        }
        assert_eq!(history.samples.len(), 30);

        // Add 40 more samples, total 70 samples. Max age is 60 secs.
        // Expect samples 0-9 to be pruned.
        for i in 30..70 {
            history.push(dummy_stats(i, i, i, i as u64));
        }
        assert_eq!(history.samples.len(), 61); // Should retain samples 9 to 69 (61 samples)

        // Verify oldest remaining sample
        assert_eq!(
            history
                .samples
                .front()
                .expect("History should not be empty")
                .timestamp,
            9
        );
        assert_eq!(
            history
                .samples
                .back()
                .expect("History should not be empty")
                .timestamp,
            69
        );

        // Test push with 0 retention (effectively disabled)
        let mut history_zero_retention = StatsHistory::new(0);
        history_zero_retention.push(dummy_stats(1, 1, 1, 1));
        history_zero_retention.push(dummy_stats(2, 2, 2, 2));
        assert_eq!(
            history_zero_retention.samples.len(),
            1,
            "0 retention should only keep the latest sample"
        );
    }

    #[test]
    fn test_aggregate_window() {
        let mut history = StatsHistory::new(100); // More than enough retention

        // Sample every second
        // Routes: 10, 20, 30, 40, 50 (timestamps 0-4)
        for i in 0..5 {
            history.push(dummy_stats((i + 1) * 10, 0, 0, i as u64));
        }
        // Current state: [t0:10, t1:20, t2:30, t3:40, t4:50]

        // Aggregate 3-second window (t2, t3, t4)
        // Values: 20, 30, 40, 50 (timestamps 1-4)
        // sum = 140, count = 4, avg = 35.0, min = 20, max = 50
        let agg_3s = history
            .aggregate(3)
            .expect("Aggregation should not be None");
        assert_eq!(agg_3s.sample_count, 4);
        assert_eq!(agg_3s.avg_routes, 35.0);
        assert_eq!(agg_3s.min_routes, 20);
        assert_eq!(agg_3s.max_routes, 50);

        // Aggregate 5-second window (all samples)
        // Values: 10, 20, 30, 40, 50
        // sum = 150, count = 5, avg = 30.0, min = 10, max = 50
        let agg_5s = history
            .aggregate(5)
            .expect("Aggregation should not be None");
        assert_eq!(agg_5s.sample_count, 5);
        assert_eq!(agg_5s.avg_routes, 30.0);
        assert_eq!(agg_5s.min_routes, 10);
        assert_eq!(agg_5s.max_routes, 50);

        // Aggregate a window before any samples
        assert!(history.aggregate(0).is_some()); // Latest sample's timestamp - 0 should include just the last one
        let last_sample_agg = history
            .aggregate(0)
            .expect("Aggregation for 0-window should not be None"); // Should be only the last sample if 0-window is used as (latest - 0)
        assert_eq!(last_sample_agg.sample_count, 1);
        assert_eq!(last_sample_agg.avg_routes, 50.0);

        // Aggregate a window larger than retention, should cover all
        let agg_large = history
            .aggregate(100)
            .expect("Aggregation for large window should not be None");
        assert_eq!(agg_large.sample_count, 5);
    }

    #[test]
    fn test_empty_history_aggregation() {
        let history = StatsHistory::new(60);
        assert!(history.aggregate(60).is_none());
    }
}
