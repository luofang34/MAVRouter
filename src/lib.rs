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
//! let config = Config::from_file("mavrouter.toml").await?;
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
#![warn(missing_docs)]  // Enable missing_docs warning

pub mod config;
pub mod router;
pub mod endpoint_core;
pub mod endpoints {
    pub mod udp;
    pub mod tcp;
    pub mod serial;
    pub mod tlog;
}
pub mod dedup;
pub mod routing;
pub mod filter;
pub mod mavlink_utils;
pub mod lock_helpers;
pub mod framing;
