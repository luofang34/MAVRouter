//! Shared endpoint orchestration logic.
//!
//! This module contains the common code for spawning MAVLink endpoints
//! and background tasks, used by both the CLI binary (`main.rs`) and the
//! library high-level API (`high_level.rs`). This eliminates ~200 lines
//! of duplicated spawning logic.

use crate::config::{Config, EndpointConfig};
use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::{EndpointStats, ExponentialBackoff};
use crate::error::Result;
use crate::filter::EndpointFilters;
use crate::router::{create_bus, EndpointId, MessageBus};
use crate::routing::RoutingTable;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Result of spawning all endpoints and background tasks.
#[allow(dead_code)] // `bus` field used by library (high_level.rs) but not binary (main.rs)
pub struct OrchestratedRouter {
    /// All spawned task handles (background tasks + endpoint supervisors).
    pub handles: Vec<JoinHandle<()>>,
    /// The message bus for inter-endpoint communication.
    pub bus: MessageBus,
    /// Shared routing table.
    pub routing_table: Arc<RwLock<RoutingTable>>,
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
    let mut rt = RoutingTable::new();

    // Register endpoint groups for redundant link support
    for (i, endpoint_config) in config.endpoint.iter().enumerate() {
        if let Some(group) = endpoint_config.group() {
            rt.set_endpoint_group(crate::router::EndpointId(i), group.to_string());
        }
    }

    let routing_table = Arc::new(RwLock::new(rt));
    let mut handles = Vec::new();
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

    // Spawn routing table pruner
    {
        let rt_prune = routing_table.clone();
        let prune_token = cancel_token.child_token();
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
    }

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

        handles.push(tokio::spawn(supervise(
            name,
            task_token.clone(),
            move || {
                let bus_tx = bus_tx.clone();
                let bus_rx = bus_tx.subscribe();
                let rt = rt.clone();
                let dd = dd.clone();
                let filters = filters.clone();
                let addr = addr.clone();
                let m = crate::config::EndpointMode::Server;
                let token = task_token.clone();
                let st = stats.clone();
                async move {
                    crate::endpoints::tcp::run(
                        id, addr, m, bus_tx, bus_rx, rt, dd, filters, token, st,
                    )
                    .await
                }
            },
        )));
    }

    // Spawn TLOG logger if configured
    if let Some(log_dir) = &config.general.log {
        if config.general.log_telemetry {
            let name = format!("TLog Logger {}", log_dir);
            let bus_tx_tlog = bus.sender();
            let dir = log_dir.clone();
            let task_token = cancel_token.child_token();

            handles.push(tokio::spawn(supervise(
                name,
                task_token.clone(),
                move || {
                    let bus_rx = bus_tx_tlog.subscribe();
                    let dir = dir.clone();
                    let token = task_token.clone();
                    async move { crate::endpoints::tlog::run(dir, bus_rx, token).await }
                },
            )));
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
                            bcast_timeout,
                            stats.clone(),
                        )
                    },
                )));
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
                            stats.clone(),
                        )
                    },
                )));
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

                handles.push(tokio::spawn(supervise(
                    name,
                    task_token.clone(),
                    move || {
                        let idx = baud_index.fetch_add(1, Ordering::Relaxed);
                        let current_baud = bauds[idx % bauds.len()];
                        crate::endpoints::serial::run(
                            i,
                            device.clone(),
                            current_baud,
                            serial_flow_control,
                            bus_tx.clone(),
                            bus.subscribe(),
                            routing_table.clone(),
                            dedup.clone(),
                            filters.clone(),
                            task_token.clone(),
                            stats.clone(),
                        )
                    },
                )));
            }
        }
    }

    OrchestratedRouter {
        handles,
        bus,
        routing_table,
        endpoint_stats,
    }
}

/// Supervisor function that restarts tasks on failure with exponential backoff.
///
/// Wraps a task factory, restarting the task whenever it completes (either
/// successfully or with an error). Uses exponential backoff to avoid rapid
/// restart loops, resetting after 60 seconds of stable operation.
pub async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30), 2.0);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} shutting down.", name);
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
                info!("Supervisor for {} shutting down during backoff.", name);
                break;
            }
        }
    }
}
