//! `RouteUpdate`: hot-path observation produced by every endpoint's ingress
//! and consumed in batches by the dedicated routing-updater task.
//!
//! Sized as `Copy` so producers can `try_send` without an `Arc` allocation;
//! the routing updater drains a batch under a single shard write lock.

use crate::router::EndpointId;
use std::time::Instant;

/// A single "endpoint X saw system/component Y at time T" observation, queued
/// for the dedicated routing updater task to apply under the write lock.
///
/// Produced on every hot-path ingress that detects the route is stale or
/// missing; consumed in batches by the routing updater task so the write
/// lock is acquired once per batch instead of once per observation.
#[derive(Debug, Clone, Copy)]
pub struct RouteUpdate {
    /// Endpoint that observed the MAVLink entity.
    pub endpoint_id: EndpointId,
    /// MAVLink system id of the observed sender.
    pub sys_id: u8,
    /// MAVLink component id of the observed sender.
    pub comp_id: u8,
    /// Monotonic timestamp of the observation.
    pub now: Instant,
}
