//! [`RoutingTable`]: the public face of the routing system.
//!
//! Wraps `SHARD_COUNT` [`super::shard::Shard`]s plus a single
//! [`super::groups::GroupConfig`]; all interior mutability lives in those
//! sub-types' `RwLock`s, so every method here takes `&self`.
//!
//! # Routing-table mutation model
//!
//! Hot ingress paths only *read* this struct (to decide whether an update
//! is needed). New observations are submitted as
//! [`super::update::RouteUpdate`]s through the routing-update channel and
//! applied by the dedicated routing-updater task; the hot path therefore
//! never blocks a tokio worker on a write lock.

use super::groups::GroupConfig;
use super::shard::{
    shard_prune, shard_remove_oldest_system, HashSet, RouteEntry, Shard, MAX_ROUTES_PER_SHARD,
    MAX_SYSTEMS_PER_SHARD, SHARD_COUNT, SHARD_MASK,
};
use super::stats::RoutingStats;
use crate::router::EndpointId;
use parking_lot::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Intelligent routing table that learns MAVLink network topology.
///
/// Routes messages based on `system_id` and `component_id`, with TTL-based
/// expiration to handle dynamic topologies. Internally sharded by
/// `system_id` so concurrent reads for different sysids don't contend, and
/// the writer task can mutate one shard without stalling readers on the
/// other 15. All public methods take `&self` — the per-shard `RwLock`s
/// provide interior mutability.
pub struct RoutingTable {
    shards: [RwLock<Shard>; SHARD_COUNT],
    config: RwLock<GroupConfig>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingTable {
    /// Creates a new, empty `RoutingTable`.
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(Shard::default())),
            config: RwLock::new(GroupConfig::default()),
        }
    }

    #[inline]
    fn shard_for(&self, sys_id: u8) -> &RwLock<Shard> {
        // `(sys_id & SHARD_MASK)` is provably < SHARD_COUNT — the mask is
        // SHARD_COUNT - 1 and SHARD_COUNT is a power of two — so the index
        // is always in bounds. Clippy can't see that.
        #[allow(clippy::indexing_slicing)]
        &self.shards[(sys_id as usize) & SHARD_MASK]
    }

    /// Registers an endpoint as belonging to a named group.
    ///
    /// Endpoints in the same group share routing knowledge, allowing
    /// redundant physical links to the same system to both forward traffic.
    pub fn set_endpoint_group(&self, id: EndpointId, group: String) {
        let mut cfg = self.config.write();
        cfg.endpoint_groups.insert(id, group.clone());
        cfg.group_members.entry(group).or_default().insert(id);
    }

    /// Sets the system IDs that trigger sniffer mode.
    pub fn set_sniffer_sysids(&self, sysids: &[u8]) {
        self.config.write().sniffer_sysids = sysids.iter().copied().collect();
    }

    /// Checks if this endpoint should receive all traffic due to sniffer mode.
    /// Returns true if any sniffer system ID has been seen on this endpoint.
    pub fn is_sniffer_endpoint(&self, endpoint_id: EndpointId) -> bool {
        // Snapshot the sniffer set first so we don't hold `config`'s lock
        // across shard lookups (and to keep the hot path lock-order
        // consistent: config before shard, never the other way round).
        let sniffers: Vec<u8> = {
            let cfg = self.config.read();
            if cfg.sniffer_sysids.is_empty() {
                return false;
            }
            cfg.sniffer_sysids.iter().copied().collect()
        };
        for sysid in sniffers {
            let shard = self.shard_for(sysid).read();
            if let Some(entry) = shard.sys_routes.get(&sysid) {
                if entry.endpoints.contains(&endpoint_id) {
                    return true;
                }
            }
        }
        false
    }

    /// Updates the routing table with a new observation.
    ///
    /// When a message is received from a MAVLink entity (`sysid`, `compid`)
    /// via a specific `endpoint_id`, this method records that the
    /// `endpoint_id` is a known path for that entity. The `last_seen`
    /// timestamp for the entry is updated.
    pub fn update(&self, endpoint_id: EndpointId, sysid: u8, compid: u8, now: Instant) {
        let shard_lock = self.shard_for(sysid);
        let mut shard = shard_lock.write();

        // Capacity-enforcement helpers. These are per-shard: the original
        // global caps (100k / 1k) are divided by `SHARD_COUNT`.
        if shard.routes.len() >= MAX_ROUTES_PER_SHARD {
            warn!(
                "Route table shard at capacity ({}), forcing prune of 60s old entries",
                MAX_ROUTES_PER_SHARD
            );
            shard_prune(&mut shard, Duration::from_secs(60));
        }

        if shard.sys_routes.len() >= MAX_SYSTEMS_PER_SHARD {
            warn!(
                "System table shard at capacity ({}), removing oldest system",
                MAX_SYSTEMS_PER_SHARD
            );
            shard_remove_oldest_system(&mut shard);
        }

        // Update routes entry for (sysid, compid)
        let mut increment_for_routes = false;
        shard
            .routes
            .entry((sysid, compid))
            .and_modify(|e| {
                if e.endpoints.insert(endpoint_id) {
                    increment_for_routes = true;
                }
                e.last_seen = now;
            })
            .or_insert_with(|| {
                increment_for_routes = true;
                RouteEntry {
                    endpoints: HashSet::from_iter([endpoint_id]),
                    last_seen: now,
                }
            });
        if increment_for_routes {
            let count = shard.endpoint_counts.entry(endpoint_id).or_insert(0);
            *count = count.saturating_add(1);
        }

        // Update sys_routes entry for sysid
        let mut increment_for_sys = false;
        shard
            .sys_routes
            .entry(sysid)
            .and_modify(|e| {
                if e.endpoints.insert(endpoint_id) {
                    increment_for_sys = true;
                }
                e.last_seen = now;
            })
            .or_insert_with(|| {
                increment_for_sys = true;
                RouteEntry {
                    endpoints: HashSet::from_iter([endpoint_id]),
                    last_seen: now,
                }
            });
        if increment_for_sys {
            let count = shard.endpoint_counts.entry(endpoint_id).or_insert(0);
            *count = count.saturating_add(1);
        }
    }

    /// Checks if an update is needed for the given route.
    ///
    /// An update is needed if the route is unknown, the endpoint isn't
    /// registered, or the last update was more than 1 second ago.
    pub fn needs_update_for_endpoint(
        &self,
        endpoint_id: EndpointId,
        sysid: u8,
        compid: u8,
        now: Instant,
    ) -> bool {
        let shard = self.shard_for(sysid).read();

        let comp_needs_update = match shard.routes.get(&(sysid, compid)) {
            None => true,
            Some(e) => {
                !e.endpoints.contains(&endpoint_id)
                    || now.duration_since(e.last_seen) >= Duration::from_secs(1)
            }
        };

        let sys_needs_update = match shard.sys_routes.get(&sysid) {
            None => true,
            Some(e) => {
                !e.endpoints.contains(&endpoint_id)
                    || now.duration_since(e.last_seen) >= Duration::from_secs(1)
            }
        };

        comp_needs_update || sys_needs_update
    }

    /// Determines if a message targeting `(target_sysid, target_compid)`
    /// should be sent to a particular `endpoint_id`.
    ///
    /// See the routing policy description at the module level for semantics.
    pub fn should_send(
        &self,
        endpoint_id: EndpointId,
        target_sysid: u8,
        target_compid: u8,
    ) -> bool {
        if self.is_sniffer_endpoint(endpoint_id) {
            return true;
        }

        if target_sysid == 0 {
            return true;
        }

        let shard = self.shard_for(target_sysid).read();
        let cfg = self.config.read();

        let Some(entry) = shard.sys_routes.get(&target_sysid) else {
            return false;
        };

        if target_compid == 0 {
            return entry.endpoints.contains(&endpoint_id)
                || cfg.any_in_same_group(&entry.endpoints, endpoint_id);
        }

        // Specific component route
        if let Some(comp_entry) = shard.routes.get(&(target_sysid, target_compid)) {
            return comp_entry.endpoints.contains(&endpoint_id)
                || cfg.any_in_same_group(&comp_entry.endpoints, endpoint_id);
        }

        // Aggressive fallback: we know the system but not this component.
        entry.endpoints.contains(&endpoint_id)
            || cfg.any_in_same_group(&entry.endpoints, endpoint_id)
    }

    /// Prunes old entries from the routing table across all shards.
    pub fn prune(&self, max_age: Duration) {
        for shard_lock in &self.shards {
            let mut shard = shard_lock.write();
            shard_prune(&mut shard, max_age);
        }
    }

    /// Returns current statistics about the routing table.
    ///
    /// This iterates all shards under read locks and unions endpoint IDs —
    /// O(shards × distinct endpoints) in the worst case, but shards is a
    /// small constant and endpoint counts are tracked incrementally per
    /// shard so the hot scan is short.
    pub fn stats(&self) -> RoutingStats {
        let mut total_systems: usize = 0;
        let mut total_routes: usize = 0;
        let mut endpoint_ids: HashSet<EndpointId> = HashSet::new();
        for shard_lock in &self.shards {
            let shard = shard_lock.read();
            total_systems = total_systems.saturating_add(shard.sys_routes.len());
            total_routes = total_routes.saturating_add(shard.routes.len());
            for ep_id in shard.endpoint_counts.keys() {
                endpoint_ids.insert(*ep_id);
            }
        }
        RoutingStats {
            total_systems,
            total_routes,
            total_endpoints: endpoint_ids.len(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}
