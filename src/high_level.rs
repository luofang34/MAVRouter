//! High-level API for starting and managing the MAVLink router.
//!
//! This module provides a simplified interface for library users who want to
//! embed mavrouter in their applications without dealing with internal details.
//!
//! # Example
//!
//! ```no_run
//! use mavrouter::Router;
//!
//! # async fn example() -> Result<(), mavrouter::error::RouterError> {
//! let toml = r#"
//! [general]
//! bus_capacity = 1000
//!
//! [[endpoint]]
//! type = "udp"
//! address = "0.0.0.0:14550"
//! mode = "server"
//! "#;
//!
//! let router = Router::from_str(toml).await?;
//! // ... use router
//! router.stop().await;
//! # Ok(())
//! # }
//! ```

use crate::config::{Config, EndpointConfig};
use crate::dedup::ConcurrentDedup;
use crate::endpoint_core::ExponentialBackoff;
use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use crate::router::{create_bus, MessageBus};
use crate::routing::RoutingTable;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// A high-level handle to a running MAVLink router.
///
/// This struct encapsulates all the complexity of setting up endpoints,
/// message bus, routing table, and background tasks. Use [`Router::start`]
/// or [`Router::from_str`] to create an instance.
///
/// # Graceful Shutdown
///
/// Call [`Router::stop`] to gracefully shut down all endpoints and background
/// tasks. The router will wait for pending operations to complete.
///
/// # Example
///
/// ```no_run
/// use mavrouter::{Router, config::Config};
///
/// # async fn example() -> Result<(), mavrouter::error::RouterError> {
/// // From a configuration file
/// let config = Config::load("mavrouter.toml").await?;
/// let router = Router::start(config).await?;
///
/// // Or from a TOML string
/// let router = Router::from_str("[general]\nbus_capacity = 1000").await?;
///
/// // Stop the router when done
/// router.stop().await;
/// # Ok(())
/// # }
/// ```
pub struct Router {
    cancel_token: CancellationToken,
    handles: Vec<JoinHandle<()>>,
    bus: MessageBus,
    routing_table: Arc<RwLock<RoutingTable>>,
}

impl Router {
    /// Starts a new router from a [`Config`] instance.
    ///
    /// This spawns all configured endpoints, background tasks (dedup rotation,
    /// routing table pruning, stats collection), and returns a handle to control
    /// the router.
    ///
    /// # Errors
    ///
    /// Returns an error if any endpoint fails to start (e.g., port already in use).
    pub async fn start(config: Config) -> Result<Self> {
        let bus = create_bus(config.general.bus_capacity);
        let routing_table = Arc::new(RwLock::new(RoutingTable::new()));
        let cancel_token = CancellationToken::new();
        let mut handles = Vec::new();

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
                        _ = dedup_token.cancelled() => break,
                        _ = interval.tick() => {
                            dedup_rotator.rotate_buckets();
                        }
                    }
                }
            }));
        }

        // Spawn routing table pruner
        let rt_prune = routing_table.clone();
        let prune_token = cancel_token.child_token();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = prune_token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(prune_interval)) => {
                        let mut rt = rt_prune.write();
                        rt.prune(Duration::from_secs(prune_ttl));
                    }
                }
            }
        }));

        // Spawn implicit TCP server if configured
        if let Some(port) = config.general.tcp_port {
            let name = format!("Implicit TCP Server :{}", port);
            let bus_tx = bus.sender();
            let bus_rx = bus.subscribe();
            let rt = routing_table.clone();
            let dd = dedup.clone();
            let id = config.endpoint.len();
            let filters = EndpointFilters::default();
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

        // Spawn TLOG logger if configured
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

        // Check for actual endpoint configuration (not just background tasks)
        let has_endpoints = !config.endpoint.is_empty() || config.general.tcp_port.is_some();
        if !has_endpoints {
            return Err(RouterError::config("No endpoints configured"));
        }

        info!(
            "Router started with {} endpoint(s) and {} background task(s)",
            config.endpoint.len(),
            handles.len()
        );

        Ok(Self {
            cancel_token,
            handles,
            bus,
            routing_table,
        })
    }

    /// Starts a router from a TOML configuration string.
    ///
    /// This is a convenience method combining [`Config::parse`] and [`Router::start`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mavrouter::Router;
    ///
    /// # async fn example() -> Result<(), mavrouter::error::RouterError> {
    /// let router = Router::from_str(r#"
    ///     [[endpoint]]
    ///     type = "udp"
    ///     address = "0.0.0.0:14550"
    /// "#).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_str(toml: &str) -> Result<Self> {
        let config = Config::parse(toml)?;
        Self::start(config).await
    }

    /// Starts a router by loading configuration from a file.
    ///
    /// This is a convenience method combining [`Config::load`] and [`Router::start`].
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let config = Config::load(path).await?;
        Self::start(config).await
    }

    /// Gracefully stops the router and all its endpoints.
    ///
    /// This method signals all tasks to stop and waits for them to complete.
    /// Pending operations (like TLOG flushing) will be given time to finish.
    pub async fn stop(self) {
        info!("Router stopping...");
        self.cancel_token.cancel();

        // Give tasks time to flush and cleanup
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Wait for all handles to complete
        for handle in self.handles {
            let _ = handle.await;
        }

        info!("Router stopped");
    }

    /// Returns a clone of the cancellation token.
    ///
    /// This can be used to integrate with external shutdown logic or
    /// to create child tokens for additional tasks.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Returns a reference to the message bus.
    ///
    /// Library users can use this to subscribe to messages or send
    /// custom messages into the router.
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }

    /// Returns a reference to the routing table.
    ///
    /// This can be used to query learned routes or inspect the
    /// network topology.
    pub fn routing_table(&self) -> &Arc<RwLock<RoutingTable>> {
        &self.routing_table
    }

    /// Checks if the router is still running.
    ///
    /// Returns `false` if [`Router::stop`] has been called or if the
    /// cancellation token has been triggered.
    pub fn is_running(&self) -> bool {
        !self.cancel_token.is_cancelled()
    }
}

/// Internal supervisor function that restarts tasks on failure.
async fn supervise<F, Fut>(name: String, cancel_token: CancellationToken, task_factory: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30), 2.0);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} shutting down", name);
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
                        warn!("Task {} finished cleanly (unexpected). Restarting...", name);
                    }
                    Err(e) => {
                        error!("Task {} failed: {:#}. Restarting...", name, e);
                    }
                }
            } => {}
        }

        let wait = backoff.next_backoff();
        info!("Waiting {:.1?} before restarting {}", wait, name);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = cancel_token.cancelled() => {
                info!("Supervisor for {} shutting down during backoff", name);
                break;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_router_from_str_no_endpoints() {
        let result = Router::from_str("").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_router_start_and_stop() {
        let toml = r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:24550"
mode = "server"
"#;
        let router = Router::from_str(toml).await.expect("should start");
        assert!(router.is_running());
        router.stop().await;
    }

    #[tokio::test]
    async fn test_router_bus_access() {
        let toml = r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:24551"
mode = "server"
"#;
        let router = Router::from_str(toml).await.expect("should start");
        let _subscriber = router.bus().subscribe();
        router.stop().await;
    }

    #[tokio::test]
    async fn test_router_routing_table_access() {
        let toml = r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:24552"
mode = "server"
"#;
        let router = Router::from_str(toml).await.expect("should start");
        let stats = router.routing_table().read().stats();
        assert_eq!(stats.total_systems, 0);
        router.stop().await;
    }
}
