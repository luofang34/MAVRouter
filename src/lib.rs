//! # mavrouter
//!
//! High-performance, intelligent MAVLink router for embedded systems.
//!
//! ## Features
//!
//! - **Intelligent Routing**: Learns network topology, routes messages efficiently
//! - **Multi-Protocol**: TCP, UDP, Serial support
//! - **Reliability**: Automatic endpoint restart, deduplication, filtering
//! - **Performance**: 50ns routing lookup, 34kHz+ throughput tested
//!
//! ## Quick Start
//!
//! The simplest way to use mavrouter as a library is with the high-level [`Router`] API:
//!
//! ```no_run
//! use mavrouter::Router;
//!
//! # async fn example() -> Result<(), mavrouter::error::RouterError> {
//! // Start from a TOML string
//! let router = Router::from_str(r#"
//!     [[endpoint]]
//!     type = "udp"
//!     address = "0.0.0.0:14550"
//!     mode = "server"
//! "#).await?;
//!
//! // Or from a configuration file
//! // let router = Router::from_file("mavrouter.toml").await?;
//!
//! // Access the message bus to subscribe to messages
//! let mut rx = router.bus().subscribe();
//!
//! // ... do your work ...
//!
//! // Gracefully shut down
//! router.stop().await;
//! # Ok(())
//! # }
//! ```
//!
//! ## Configuration
//!
//! See [`config::Config`] for TOML configuration structure. You can also create
//! configurations programmatically using [`config::Config::parse`] or the
//! standard [`FromStr`](std::str::FromStr) trait.
//!
//! ## Re-exported Dependencies
//!
//! For convenience, commonly needed dependencies are re-exported:
//!
//! - [`CancellationToken`]: For graceful shutdown control
//! - [`RwLock`]: Used by the routing table (from `parking_lot`)
//!
//! ## Architecture
//!
//! - [`Router`]: High-level API for starting and managing the router
//! - [`router`]: Low-level message bus and routing primitives
//! - [`routing`]: Intelligent routing table
//! - [`endpoints`]: TCP/UDP/Serial endpoint implementations
//! - [`filter`]: Per-endpoint message filtering
//! - [`config`]: Configuration parsing and validation

#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(missing_docs)] // Enforce 100% documentation coverage

/// Router configuration and parsing utilities.
pub mod config;
/// Core logic for generic endpoint operations.
pub mod endpoint_core;
/// Custom error types for structured error handling.
pub mod error;
mod high_level;
/// Core message routing logic and types.
pub mod router;
/// Various MAVLink endpoint implementations (TCP, UDP, Serial, TLOG).
pub mod endpoints {
    /// Serial endpoint specific implementation.
    pub mod serial;
    /// TCP endpoint specific implementation.
    pub mod tcp;
    /// TLOG (Telemetry Log) endpoint for message recording.
    pub mod tlog;
    /// UDP endpoint specific implementation.
    pub mod udp;
}
/// Message deduplication logic.
pub mod dedup;
/// Message filtering capabilities for endpoints.
pub mod filter;
/// MAVLink message framing and parsing from byte streams.
pub mod framing;
/// Utility functions for MAVLink message processing.
pub mod mavlink_utils;
/// Routing table implementation for MAVLink messages.
pub mod routing;
/// Statistics history tracking and aggregation.
pub mod stats;

// Re-export commonly needed dependencies for library users.
// This allows users to avoid adding these as direct dependencies
// and ensures version compatibility.

/// Re-export of `tokio_util::sync::CancellationToken` for graceful shutdown control.
pub use tokio_util::sync::CancellationToken;

/// Re-export of `parking_lot::RwLock` used by the routing table.
pub use parking_lot::RwLock;

// Re-export the high-level Router API at crate root for convenience.
pub use high_level::Router;
