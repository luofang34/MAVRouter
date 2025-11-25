//! # mavrouter-rs
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
//! ```no_run
//! use mavrouter_rs::{config::Config, router};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = Config::load("mavrouter.toml").await?;
//! let bus = router::create_bus(1000);
//! // Start endpoints...
//! # Ok(())
//! # }
//! ```
//!
//! ## Configuration
//!
//! See [`config::Config`] for TOML configuration structure.
//!
//! ## Architecture
//!
//! - [`router`]: Message bus and routing
//! - [`routing`]: Intelligent routing table
//! - [`endpoints`]: TCP/UDP/Serial endpoint implementations
//! - [`filter`]: Per-endpoint message filtering

#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(missing_docs)] // Enforce 100% documentation coverage

/// Router configuration and parsing utilities.
pub mod config;
/// Core logic for generic endpoint operations.
pub mod endpoint_core;
/// Custom error types for structured error handling.
pub mod error;
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
