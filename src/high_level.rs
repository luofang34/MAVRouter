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

use crate::config::Config;
use crate::endpoint_core::EndpointStats;
use crate::error::{Result, RouterError};
use crate::orchestration::{shutdown_with_timeout, NamedTask};
use crate::router::{EndpointId, MessageBus};
use crate::routing::RoutingTable;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
    tasks: Vec<NamedTask>,
    bus: MessageBus,
    routing_table: Arc<RoutingTable>,
    endpoint_stats: Vec<(EndpointId, String, Arc<EndpointStats>)>,
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
    /// Returns an error if no endpoints are configured.
    pub async fn start(config: Config) -> Result<Self> {
        // Check for endpoint configuration before spawning any tasks
        let has_endpoints = !config.endpoint.is_empty() || config.general.tcp_port.is_some();
        if !has_endpoints {
            return Err(RouterError::config("No endpoints configured"));
        }

        let cancel_token = CancellationToken::new();
        let orchestrated = crate::orchestration::spawn_all(&config, &cancel_token);

        info!(
            "Router started with {} endpoint(s) and {} background task(s)",
            config.endpoint.len(),
            orchestrated.tasks.len()
        );

        Ok(Self {
            cancel_token,
            tasks: orchestrated.tasks,
            bus: orchestrated.bus,
            routing_table: orchestrated.routing_table,
            endpoint_stats: orchestrated.endpoint_stats,
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
    /// Signals all tasks to stop and waits up to 5 seconds for them to
    /// finish. If any task fails to exit within the budget, its name is
    /// logged at `error!` and it is aborted so `stop()` is guaranteed to
    /// return in bounded time even if a task is misbehaving.
    pub async fn stop(self) {
        info!("Router stopping...");
        self.cancel_token.cancel();
        shutdown_with_timeout(self.tasks, Duration::from_secs(5)).await;
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
    pub fn routing_table(&self) -> &Arc<RoutingTable> {
        &self.routing_table
    }

    /// Returns per-endpoint traffic statistics.
    ///
    /// Each entry contains the endpoint ID, a human-readable name, and
    /// a shared reference to the atomic stats counters. Call
    /// [`EndpointStats::snapshot`] to obtain a point-in-time view.
    pub fn endpoint_stats(&self) -> &[(EndpointId, String, Arc<EndpointStats>)] {
        &self.endpoint_stats
    }

    /// Checks if the router is still running.
    ///
    /// Returns `false` if [`Router::stop`] has been called or if the
    /// cancellation token has been triggered.
    pub fn is_running(&self) -> bool {
        !self.cancel_token.is_cancelled()
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
    async fn test_router_endpoint_stats() {
        let toml = r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:24560"
mode = "server"

[[endpoint]]
type = "udp"
address = "127.0.0.1:24561"
mode = "server"
"#;
        let router = Router::from_str(toml).await.expect("should start");
        let stats = router.endpoint_stats();
        // Should have at least 2 entries for our 2 endpoints
        assert!(
            stats.len() >= 2,
            "expected at least 2 endpoint stats entries, got {}",
            stats.len()
        );
        router.stop().await;
    }

    #[tokio::test]
    async fn test_router_is_running() {
        let toml = r#"
[[endpoint]]
type = "udp"
address = "127.0.0.1:24562"
mode = "server"
"#;
        let router = Router::from_str(toml).await.expect("should start");
        assert!(router.is_running(), "router should be running after start");

        // Obtain a token clone before stopping
        let token = router.cancel_token();
        assert!(!token.is_cancelled());

        router.stop().await;
        // After stop, the token should be cancelled
        assert!(token.is_cancelled());
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
        let stats = router.routing_table().stats();
        assert_eq!(stats.total_systems, 0);
        router.stop().await;
    }
}
