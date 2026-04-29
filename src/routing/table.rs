//! [`RoutingTable`]: the public face of the routing system.
//!
//! Wraps `SHARD_COUNT` [`super::shard::Shard`]s plus a single
//! [`super::groups::GroupConfig`] snapshot. Shards are guarded by
//! `parking_lot::RwLock` (short, sharded, read-hot) and the group config
//! is held in an [`arc_swap::ArcSwap`] so reads are lock-free atomic
//! loads — crucial for hot ingress / egress paths that must not park a
//! tokio worker on a contended `RwLock`.
//!
//! # Lock discipline (hot path)
//!
//! The hot egress path ([`RoutingTable::should_send`]) MUST NOT hold two
//! synchronisation primitives simultaneously, and MUST NOT hold the
//! group-config across a shard acquisition — doing either would (a) risk
//! parking a tokio worker behind the single global `RwLock<GroupConfig>`
//! under contention and (b) invert lock ordering against
//! [`RoutingTable::is_sniffer_endpoint`], which could latch into a
//! deadlock the moment group-config becomes contended. The discipline:
//!
//! 1. **Snapshot** the group config first with `ArcSwap::load` — lock-free.
//! 2. **Then** acquire at most one shard read lock and do all shard work.
//! 3. **Never** re-acquire the config after touching shards.
//!
//! `load()` returns a short-lived `Guard` that owns an `Arc`; holding it
//! across a shard `.read()` is fine because it is *not* a lock — there's
//! no contention, no parking.
//!
//! # Routing-table mutation model
//!
//! Hot ingress paths only *read* this struct (to decide whether an update
//! is needed). New observations are submitted as
//! [`super::update::RouteUpdate`]s through the routing-update channel and
//! applied by the dedicated routing-updater task; the hot path therefore
//! never blocks a tokio worker on a write lock. Group-config writes
//! (`set_endpoint_group`, `set_sniffer_sysids`) are copy-on-write: load
//! the current snapshot, mutate a clone, `store` the new Arc. Writes are
//! rare (called at startup for group registration and sniffer setup).

use super::groups::GroupConfig;
use super::shard::{
    shard_prune, shard_remove_oldest_system, HashSet, RouteEntry, Shard, MAX_ROUTES_PER_SHARD,
    MAX_SYSTEMS_PER_SHARD, SHARD_COUNT, SHARD_MASK,
};
use super::stats::RoutingStats;
use crate::router::EndpointId;
use arc_swap::ArcSwap;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Intelligent routing table that learns MAVLink network topology.
///
/// Routes messages based on `system_id` and `component_id`, with TTL-based
/// expiration to handle dynamic topologies. Internally sharded by
/// `system_id` so concurrent reads for different sysids don't contend, and
/// the writer task can mutate one shard without stalling readers on the
/// other 15. The group-config (endpoint groups + sniffer sysids) is held
/// in an [`ArcSwap`] so hot-path reads never take a lock. All public
/// methods take `&self`; the per-shard `RwLock`s and the `ArcSwap`
/// provide interior mutability.
pub struct RoutingTable {
    shards: [RwLock<Shard>; SHARD_COUNT],
    /// Group / sniffer configuration. Held behind [`ArcSwap`] rather than
    /// a `RwLock` so the hot path takes a lock-free atomic load instead
    /// of parking a tokio worker thread on a contended lock. Writes are
    /// copy-on-write via [`ArcSwap::rcu`] or load/clone/store.
    config: ArcSwap<GroupConfig>,
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
            config: ArcSwap::from_pointee(GroupConfig::default()),
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

    /// Copy-on-write mutation of the group config. Loads the current
    /// snapshot, hands a mutable clone to `edit`, and atomically
    /// publishes the new snapshot. Writers never block readers.
    fn rcu_config<F: FnMut(&mut GroupConfig)>(&self, mut edit: F) {
        // `rcu` re-runs `edit` on a fresh snapshot if a concurrent writer
        // raced us; concurrent writers are rare (startup only) so the
        // retry cost is negligible.
        self.config.rcu(|current| {
            let mut next = GroupConfig::clone(current);
            edit(&mut next);
            Arc::new(next)
        });
    }

    /// Registers an endpoint as belonging to a named group.
    ///
    /// Endpoints in the same group share routing knowledge, allowing
    /// redundant physical links to the same system to both forward traffic.
    pub fn set_endpoint_group(&self, id: EndpointId, group: String) {
        self.rcu_config(|cfg| {
            cfg.endpoint_groups.insert(id, group.clone());
            cfg.group_members
                .entry(group.clone())
                .or_default()
                .insert(id);
        });
    }

    /// Sets the system IDs that trigger sniffer mode.
    pub fn set_sniffer_sysids(&self, sysids: &[u8]) {
        let new: super::shard::HashSet<u8> = sysids.iter().copied().collect();
        self.rcu_config(|cfg| cfg.sniffer_sysids = new.clone());
    }

    /// Checks if this endpoint should receive all traffic due to sniffer mode.
    /// Returns true if any sniffer system ID has been seen on this endpoint.
    pub fn is_sniffer_endpoint(&self, endpoint_id: EndpointId) -> bool {
        // Lock-free snapshot of the group config — `ArcSwap::load` is an
        // atomic pointer load, not a lock acquisition. Safe to hold
        // across shard reads because it can never park a tokio worker.
        let cfg = self.config.load();
        if cfg.sniffer_sysids.is_empty() {
            return false;
        }
        for &sysid in cfg.sniffer_sysids.iter() {
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
    /// See the routing policy description at the module level for
    /// semantics. Hot-path: the group-config snapshot is taken via a
    /// lock-free [`ArcSwap::load`] BEFORE any shard read, so we never
    /// hold two synchronisation primitives simultaneously on the
    /// egress path.
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

        // Snapshot the group-config lock-free BEFORE touching any shard.
        // Holding `cfg` across the shard read is fine: `ArcSwap::load`
        // returns a refcount-guarded `Arc` view, not a lock, so it can
        // never park a tokio worker or participate in a lock cycle.
        let cfg = self.config.load();
        let shard = self.shard_for(target_sysid).read();

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
