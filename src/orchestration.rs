//! Shared endpoint orchestration logic.
//!
//! This module contains the common code for spawning MAVLink endpoints
//! and background tasks, used by both the CLI binary (`main.rs`) and the
//! library high-level API (`high_level.rs`). This eliminates ~200 lines
//! of duplicated spawning logic.

use crate::config::{Config, EndpointConfig};
use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::EndpointStats;
use crate::filter::EndpointFilters;
use crate::router::{create_bus, EndpointId, MessageBus};
use crate::routing::{RouteUpdate, RoutingTable};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Bounded capacity of the routing-update channel that feeds the routing
/// updater task from every endpoint's ingress path.
///
/// Sized to absorb bursts (e.g. startup, when every endpoint simultaneously
/// learns about every `sys_id` on the network) while keeping memory bounded
/// — each queued [`RouteUpdate`] is ~24 bytes, so 4096 entries is < 100 KiB.
/// Under sustained overflow the producers drop observations (logged at
/// `trace!`) and retry on the next ingress, because `needs_update_for_endpoint`
/// self-refreshes every second.
const ROUTE_UPDATE_QUEUE_CAPACITY: usize = 4096;

/// Maximum number of queued [`RouteUpdate`]s the updater drains per batch
/// before releasing the routing-table write lock. Larger batches amortise
/// lock acquisition; too-large batches extend the worst-case read latency.
const ROUTE_UPDATE_BATCH_SIZE: usize = 256;

/// A spawned task tagged with a human-readable name.
///
/// The name is used by shutdown diagnostics to report which tasks
/// failed to exit within the shutdown budget.
pub struct NamedTask {
    /// Human-readable name (e.g. `"UDP Endpoint 0 (0.0.0.0:14550)"`).
    pub name: String,
    /// Join handle for the spawned task.
    pub handle: JoinHandle<()>,
}

impl NamedTask {
    /// Wrap a spawned task with a descriptive name.
    pub fn new(name: impl Into<String>, handle: JoinHandle<()>) -> Self {
        Self {
            name: name.into(),
            handle,
        }
    }
}

/// Result of spawning all endpoints and background tasks.
#[allow(dead_code)] // `bus` field used by library (high_level.rs) but not binary (main.rs)
pub struct OrchestratedRouter {
    /// All spawned tasks, each tagged with a name for shutdown diagnostics.
    pub tasks: Vec<NamedTask>,
    /// The message bus for inter-endpoint communication.
    pub bus: MessageBus,
    /// Shared routing table.
    pub routing_table: Arc<RoutingTable>,
    /// Per-endpoint statistics: (EndpointId, name, stats).
    pub endpoint_stats: Vec<(EndpointId, String, Arc<EndpointStats>)>,
}

/// Spawns all endpoints and background tasks from a configuration.
///
/// Creates the message bus, routing table, and dedup instance, then spawns:
/// - Dedup rotator (if dedup is enabled)
/// - Routing table pruner
/// - Implicit TCP server (if `tcp_port` is configured)
/// - TLOG logger (if logging is configured)
/// - All configured endpoints (UDP, TCP, Serial) with supervisors
pub fn spawn_all(config: &Config, cancel_token: &CancellationToken) -> OrchestratedRouter {
    let bus = create_bus(config.general.bus_capacity);
    let rt = RoutingTable::new();

    // Register endpoint groups for redundant link support
    for (i, endpoint_config) in config.endpoint.iter().enumerate() {
        if let Some(group) = endpoint_config.group() {
            rt.set_endpoint_group(crate::router::EndpointId(i), group.to_string());
        }
    }

    // Register sniffer system IDs
    if !config.general.sniffer_sysids.is_empty() {
        rt.set_sniffer_sysids(&config.general.sniffer_sysids);
    }

    let routing_table = Arc::new(rt);
    let mut tasks: Vec<NamedTask> = Vec::new();
    let mut endpoint_stats: Vec<(EndpointId, String, Arc<EndpointStats>)> = Vec::new();

    let dedup_period = config.general.dedup_period_ms.unwrap_or(0);
    let dedup = ConcurrentDedup::new(Duration::from_millis(dedup_period));

    let prune_ttl = config.general.routing_table_ttl_secs;
    let prune_interval = config.general.routing_table_prune_interval_secs;

    // Spawn dedup rotator if enabled
    let dedup_rotation_interval = dedup.rotation_interval();
    if !dedup_rotation_interval.is_zero() {
        let dedup_rotator = dedup.clone();
        let dedup_token = cancel_token.child_token();
        tasks.push(NamedTask::new(
            "Dedup Rotator",
            tokio::spawn(async move {
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
            }),
        ));
    }

    // Spawn the single routing updater task. It owns *all* writes to the
    // routing table: streamed route observations submitted by endpoints via
    // the `route_update_tx` channel, and periodic prune cycles. This is the
    // only code path in the crate that holds `routing_table.as_ref()` — the
    // ingress hot path is strictly non-blocking.
    let (route_update_tx, route_update_rx) =
        mpsc::channel::<RouteUpdate>(ROUTE_UPDATE_QUEUE_CAPACITY);
    tasks.push(spawn_routing_updater(
        routing_table.clone(),
        route_update_rx,
        Duration::from_secs(prune_ttl),
        Duration::from_secs(prune_interval),
        cancel_token.child_token(),
    ));

    // Spawn implicit TCP server if configured
    if let Some(port) = config.general.tcp_port {
        let name = format!("Implicit TCP Server :{}", port);
        let stats = Arc::new(EndpointStats::new());
        let bus_tx = bus.sender();
        let rt = routing_table.clone();
        let dd = dedup.clone();
        let id = config.endpoint.len();
        let filters = EndpointFilters::default();
        let addr = format!("0.0.0.0:{}", port);
        let task_token = cancel_token.child_token();

        endpoint_stats.push((EndpointId(id), name.clone(), stats.clone()));

        let supervisor_name = name.clone();
        let route_tx = route_update_tx.clone();
        tasks.push(NamedTask::new(
            name,
            tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                let bus_tx = bus_tx.clone();
                let bus_rx = bus_tx.subscribe();
                let rt = rt.clone();
                let dd = dd.clone();
                let filters = filters.clone();
                let addr = addr.clone();
                let m = crate::config::EndpointMode::Server;
                let token = task_token.clone();
                let st = stats.clone();
                let route_tx = route_tx.clone();
                async move {
                    crate::endpoints::tcp::run(
                        id, addr, m, bus_tx, bus_rx, rt, route_tx, dd, filters, token, st,
                    )
                    .await
                }
            })),
        ));
    }

    // Spawn TLOG logger if configured
    if let Some(log_dir) = &config.general.log {
        if config.general.log_telemetry {
            let name = format!("TLog Logger {}", log_dir);
            let bus_tx_tlog = bus.sender();
            let dir = log_dir.clone();
            let task_token = cancel_token.child_token();

            let supervisor_name = name.clone();
            tasks.push(NamedTask::new(
                name,
                tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                    let bus_rx = bus_tx_tlog.subscribe();
                    let dir = dir.clone();
                    let token = task_token.clone();
                    async move { crate::endpoints::tlog::run(dir, bus_rx, token).await }
                })),
            ));
        }
    }

    // Spawn configured endpoints
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
                broadcast_timeout_secs,
                ..
            } => {
                let name = format!("UDP Endpoint {} ({})", i, address);
                let stats = Arc::new(EndpointStats::new());
                endpoint_stats.push((EndpointId(i), name.clone(), stats.clone()));
                let address = address.clone();
                let mode = mode.clone();
                let filters = filters.clone();
                let cleanup_ttl = prune_ttl;
                let bcast_timeout = *broadcast_timeout_secs;

                let supervisor_name = name.clone();
                let route_tx = route_update_tx.clone();
                tasks.push(NamedTask::new(
                    name,
                    tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                        crate::endpoints::udp::run(
                            i,
                            address.clone(),
                            mode.clone(),
                            bus_tx.clone(),
                            bus.subscribe(),
                            routing_table.clone(),
                            route_tx.clone(),
                            dedup.clone(),
                            filters.clone(),
                            task_token.clone(),
                            cleanup_ttl,
                            bcast_timeout,
                            stats.clone(),
                        )
                    })),
                ));
            }
            EndpointConfig::Tcp {
                address,
                mode,
                filters,
                ..
            } => {
                let name = format!("TCP Endpoint {} ({})", i, address);
                let stats = Arc::new(EndpointStats::new());
                endpoint_stats.push((EndpointId(i), name.clone(), stats.clone()));
                let address = address.clone();
                let mode = mode.clone();
                let filters = filters.clone();

                let supervisor_name = name.clone();
                let route_tx = route_update_tx.clone();
                tasks.push(NamedTask::new(
                    name,
                    tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                        crate::endpoints::tcp::run(
                            i,
                            address.clone(),
                            mode.clone(),
                            bus_tx.clone(),
                            bus.subscribe(),
                            routing_table.clone(),
                            route_tx.clone(),
                            dedup.clone(),
                            filters.clone(),
                            task_token.clone(),
                            stats.clone(),
                        )
                    })),
                ));
            }
            EndpointConfig::Serial {
                device,
                baud,
                flow_control,
                filters,
                ..
            } => {
                let name = format!("Serial Endpoint {} ({})", i, device);
                let stats = Arc::new(EndpointStats::new());
                endpoint_stats.push((EndpointId(i), name.clone(), stats.clone()));
                let device = device.clone();
                let bauds: Vec<u32> = baud.rates().to_vec();
                let baud_index = Arc::new(AtomicUsize::new(0));
                let serial_flow_control = match flow_control {
                    crate::config::FlowControl::None => tokio_serial::FlowControl::None,
                    crate::config::FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
                    crate::config::FlowControl::Software => tokio_serial::FlowControl::Software,
                };
                let filters = filters.clone();

                let supervisor_name = name.clone();
                let route_tx = route_update_tx.clone();
                tasks.push(NamedTask::new(
                    name,
                    tokio::spawn(supervise(supervisor_name, task_token.clone(), move || {
                        let idx = baud_index.fetch_add(1, Ordering::Relaxed);
                        // idx % bauds.len() is always in bounds; bauds is non-empty (validated by config)
                        #[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
                        let current_baud = bauds[idx % bauds.len()];
                        crate::endpoints::serial::run(
                            i,
                            device.clone(),
                            current_baud,
                            serial_flow_control,
                            bus_tx.clone(),
                            bus.subscribe(),
                            routing_table.clone(),
                            route_tx.clone(),
                            dedup.clone(),
                            filters.clone(),
                            task_token.clone(),
                            stats.clone(),
                        )
                    })),
                ));
            }
        }
    }
    drop(route_update_tx);

    OrchestratedRouter {
        tasks,
        bus,
        routing_table,
        endpoint_stats,
    }
}

#[cfg(test)]
mod tests;

mod shutdown;
mod supervisor;
mod updater;

// `shutdown_with_timeout` is used by the library's `high_level::Router`
// and by integration tests; the binary shuts down inline, so the bin target
// reports the re-export as unused. Allow it — this is the intended contract.
#[allow(unused_imports)]
pub use shutdown::shutdown_with_timeout;
pub use supervisor::supervise;
pub use updater::spawn_routing_updater;
