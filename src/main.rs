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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

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

        // In a real ring buffer we could optimize this, but iteration is fine for small N
        // or even 86400 items (linear scan is fast in memory).
        // Optimization: Find start index using binary search or reverse iterator if needed.
        // For <100k items, iter is usually fine.
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
    #[allow(dead_code)]
    min_routes: usize,
    sample_count: usize,
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

                        // Add timestamp if not already set by routing (it is set now, but ensure consistency)
                        // routing.rs stats() sets it, so we can use it directly or overwrite to be sure of sampling time.
                        // Let's overwrite to capture "sample time" rather than "lock time" if slightly different,
                        // though routing.rs uses SystemTime::now() too.
                        stats.timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        history.push(stats.clone()); // Clone is cheap (3 usizes + u64)

                        // Periodic logging
                        if stats.timestamp.saturating_sub(last_log_time) >= log_interval {
                            // 1 minute aggregation
                            if let Some(min1) = history.aggregate(60) {
                                info!(
                                    "Stats [1min] avg={:.1} routes, max={}",
                                    min1.avg_routes, min1.max_routes
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

                            last_log_time = stats.timestamp;
                        }
                    }
                }
            }
        }));
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
