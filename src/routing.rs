//! Intelligent routing table for MAVLink message forwarding.
//!
//! The routing layer learns MAVLink network topology from observed traffic
//! and routes by `system_id` / `component_id`, with TTL-based expiration to
//! handle dynamic topologies. The public surface is intentionally narrow:
//!
//! - [`RoutingTable`] — the routing table itself (interior mutability via
//!   per-shard `RwLock`s).
//! - [`RoutingStats`] — point-in-time snapshot for the metrics pipeline.
//! - `RouteUpdate` — hot-path observation queued by endpoints for the
//!   dedicated routing-updater task.
//!
//! Internal layout follows the `foo.rs` + `foo/` convention required by the
//! crate (no `mod.rs`):
//!
//! - `table` — [`RoutingTable`] struct and its `impl`.
//! - `shard` — per-`system_id` shard (constants, `Shard`, `RouteEntry`,
//!   prune/eviction helpers).
//! - `groups` — endpoint-group / sniffer-sysid policy.
//! - `stats` — [`RoutingStats`] snapshot type.
//! - `update` — `RouteUpdate` hot-path message.
//!
//! # Routing-table mutation model
//!
//! The hot ingress path only *reads* the routing table (to decide whether an
//! update is needed). New observations are submitted to a dedicated
//! "Routing Updater" background task through a bounded mpsc channel. This
//! keeps the hot path strictly non-blocking: it never calls `RwLock::write()`
//! itself, which would stall a tokio worker thread under contention.

mod groups;
mod shard;
mod stats;
mod table;
mod update;

#[cfg(test)]
mod tests;

pub use stats::RoutingStats;
pub use table::RoutingTable;
pub use update::RouteUpdate;
