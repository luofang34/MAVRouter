//! Periodic routing-table statistics sampler + aggregation logger.
//!
//! Samples the routing table every `sample_interval` seconds, retains
//! up to `retention` seconds of history, and emits aggregated stats at
//! every `log_interval`. Lives here (not in `main.rs`) so the shutdown
//! story stays in one place — the returned [`NamedTask`] goes into the
//! same join list as every other orchestrated task.

// Used only by the binary (`main.rs`); the library never spawns this.
#![allow(dead_code)]

use super::stats_history::StatsHistory;
use super::NamedTask;
use crate::endpoint_core::EndpointStats;
use crate::router::EndpointId;
use crate::routing::RoutingTable;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Spawn the stats-reporter task.
///
/// Returns `None` when stats are disabled — either `sample_interval`
/// or `retention` is zero. Matches the pre-extraction gating in
/// `main.rs` exactly so behaviour doesn't drift.
pub fn spawn_stats_reporter(
    routing_table: Arc<RoutingTable>,
    endpoint_stats: Vec<(EndpointId, String, Arc<EndpointStats>)>,
    sample_interval: u64,
    retention: u64,
    log_interval: u64,
    cancel_token: CancellationToken,
) -> Option<NamedTask> {
    if sample_interval == 0 || retention == 0 {
        return None;
    }
    let handle = tokio::spawn(async move {
        let mut history = StatsHistory::new(retention);
        let mut last_log_time = 0u64;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Stats Reporter shutting down.");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(sample_interval)) => {
                    let mut stats = routing_table.stats();
                    stats.timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
                        Ok(d) => d.as_secs(),
                        Err(e) => {
                            // System clock predates the UNIX epoch — can happen
                            // on embedded boards with an unset RTC. Record 0 and
                            // warn so the anomaly shows up rather than being
                            // swallowed.
                            warn!("System clock is before UNIX epoch: {}", e);
                            0
                        }
                    };

                    let current_timestamp = stats.timestamp;
                    history.push(stats.clone());

                    if current_timestamp.saturating_sub(last_log_time) >= log_interval {
                        if let Some(min1) = history.aggregate(60) {
                            info!(
                                "Stats [1min] avg={:.1} routes, range=[{}-{}]",
                                min1.avg_routes, min1.min_routes, min1.max_routes
                            );
                        }
                        if let Some(hour1) = history.aggregate(3600) {
                            info!(
                                "Stats [1hr] avg={:.1} routes, max={}",
                                hour1.avg_routes, hour1.max_routes
                            );
                        }
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
                        for (ep_id, ep_name, ep_stats) in &endpoint_stats {
                            let snap = ep_stats.snapshot();
                            info!("Endpoint {} ({}) {}", ep_id, ep_name, snap);
                        }

                        last_log_time = current_timestamp;
                    }
                }
            }
        }
    });
    Some(NamedTask::new("Stats Reporter", handle))
}
