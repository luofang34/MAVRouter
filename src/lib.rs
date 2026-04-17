//! # mavrouter
//!
//! MAVLink router: learns network topology from traffic, routes by
//! `system_id`/`component_id`, and supports Serial, UDP, and TCP endpoints
//! in either client or server mode. See the [`Router`] handle for the
//! entry points.
//!
//! ## Public API surface
//!
//! The library exposes a small, opinionated surface. For nearly every use
//! case, the only items you need are:
//!
//! - [`Router`] — start, stop, and interact with a running router.
//! - [`config::Config`] — parse or construct a configuration.
//! - [`error::RouterError`] / [`error::Result`] — the crate's error type.
//!
//! Everything else is internal. If you find yourself wanting a type from
//! one of the private submodules, open an issue so the missing capability
//! can be added to the public surface instead of leaking implementation
//! details.
//!
//! ## Quick Start
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
//! See [`config::Config`] for the TOML configuration structure. You can also
//! create configurations programmatically via [`config::Config::parse`] or
//! the standard [`FromStr`](std::str::FromStr) trait.

#![deny(missing_docs)] // Enforce 100% documentation coverage (lib-only, not in Cargo.toml)

/// Router configuration and parsing utilities.
pub mod config;
/// Custom error types for structured error handling.
pub mod error;

// ---------------------------------------------------------------------------
// Internal modules. All of these are `pub(crate)` by design: the crate's
// public API is limited to `Router`, `Config`, and `error::*`. Tests live
// inside these modules as `#[cfg(test)] mod tests;` so they have direct
// access to internals; integration tests in `tests/` can only use the
// public surface.
// ---------------------------------------------------------------------------

// `_internal` gates the handful of modules that the in-crate benchmarks
// need direct access to. It is a *development-only* feature — not part of
// the stable public contract — and must never be enabled by downstream
// users. Each `[[bench]]` target sets `required-features = ["_internal"]`
// so `cargo bench` turns it on automatically; every other build path sees
// the strict `pub(crate)` visibility.

pub(crate) mod dedup;
pub(crate) mod endpoint_core;
pub(crate) mod endpoints {
    pub(crate) mod serial;
    pub(crate) mod tcp;
    pub(crate) mod tlog;
    pub(crate) mod udp;
}
/// Per-endpoint message filtering. Exposed only under the `_internal`
/// feature for benchmark access; not part of the public API.
#[cfg(feature = "_internal")]
pub mod filter;
#[cfg(not(feature = "_internal"))]
pub(crate) mod filter;
pub(crate) mod framing;
mod high_level;
/// Low-level MAVLink message utilities. Exposed only under the `_internal`
/// feature for benchmark access; not part of the public API.
#[cfg(feature = "_internal")]
pub mod mavlink_utils;
#[cfg(not(feature = "_internal"))]
pub(crate) mod mavlink_utils;
pub(crate) mod orchestration;
/// Message bus primitives. Exposed only under the `_internal` feature for
/// benchmark access; not part of the public API. Use [`Router::bus`] instead.
#[cfg(feature = "_internal")]
pub mod router;
#[cfg(not(feature = "_internal"))]
pub(crate) mod router;
/// Routing table internals. Exposed only under the `_internal` feature for
/// benchmark access; not part of the public API. Query routing state via
/// [`Router::routing_table`] or [`RoutingStats`] instead.
#[cfg(feature = "_internal")]
pub mod routing;
#[cfg(not(feature = "_internal"))]
pub(crate) mod routing;

// Re-export the high-level Router API at crate root for convenience.
pub use high_level::Router;

// Supporting types reachable via `Router`'s public methods. These are
// necessary — and minimal — because each one appears in the signature of
// a `Router` accessor and downstream users would otherwise not be able to
// name the return type. Any addition here widens the public API and
// should go through the `cargo public-api` diff gate.
pub use endpoint_core::{EndpointStats, EndpointStatsSnapshot};
pub use mavlink_utils::MessageTarget;
pub use router::{EndpointId, MessageBus, RoutedMessage};
pub use routing::{RoutingStats, RoutingTable};
