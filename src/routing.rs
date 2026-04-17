use crate::router::EndpointId;
use ahash::{AHashMap, AHashSet};
use parking_lot::RwLock;
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

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

// Type aliases using ahash for faster hashing
type HashMap<K, V> = AHashMap<K, V>;
type HashSet<T> = AHashSet<T>;

/// Number of shards the routing table is partitioned into, keyed by
/// `system_id & SHARD_MASK`.
///
/// 16 shards is the sweet spot on the current workloads: under concurrent
/// read+write load, reads for `sys_id=5` don't stall behind a write for
/// `sys_id=7`, while the per-shard memory overhead stays negligible.
/// `system_id` is a `u8`, so 16 shards puts each shard responsible for
/// 16 ids on average — well distributed even for small installations.
const SHARD_COUNT: usize = 16;
const SHARD_MASK: usize = SHARD_COUNT - 1;

// Constants for routing table capacity limits. Both are per-shard so the
// total addressable capacity scales with `SHARD_COUNT`.
const MAX_ROUTES_PER_SHARD: usize = 100_000 / SHARD_COUNT;
const MAX_SYSTEMS_PER_SHARD: usize = 1_000 / SHARD_COUNT;

/// Represents an entry in the routing table for a specific (system_id, component_id) pair
/// or just a system_id. It tracks which endpoints have seen this MAVLink entity.
struct RouteEntry {
    /// Set of endpoint IDs that have seen this MAVLink entity.
    endpoints: HashSet<EndpointId>,
    /// The last time this entry was updated or a message was seen from this entity.
    last_seen: Instant,
}

/// Per-shard state: only the routes whose `system_id` hashes to this shard.
/// Each shard holds an independent `RwLock`, so read traffic on different
/// sysids doesn't serialise through a single writer.
#[derive(Default)]
struct Shard {
    /// Map of `(system_id, component_id)` -> `RouteEntry` for specific component routes.
    /// All keys here satisfy `system_id as usize & SHARD_MASK == <this shard index>`.
    routes: HashMap<(u8, u8), RouteEntry>,
    /// Map of `system_id` -> `RouteEntry` for system-wide routes (component_id 0).
    sys_routes: HashMap<u8, RouteEntry>,
    /// Incremental count of how many active route entries each EndpointId is
    /// present in *within this shard*. The global count (used by stats) is
    /// the set-union of keys across all shards, not the sum of values.
    endpoint_counts: HashMap<EndpointId, usize>,
}

/// Group / sniffer configuration. Mostly written once at startup and read
/// (rarely) on the hot path; held behind a single `RwLock` because
/// sharding it by anything meaningful would cost more than it saves.
#[derive(Default)]
struct Config {
    /// Map of endpoint ID to its group name (if any).
    endpoint_groups: HashMap<EndpointId, String>,
    /// Reverse lookup: group name -> set of endpoint IDs in that group.
    group_members: HashMap<String, HashSet<EndpointId>>,
    /// Set of system IDs that trigger sniffer mode.
    sniffer_sysids: HashSet<u8>,
}

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
    config: RwLock<Config>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the current state of the routing table.
#[derive(Debug, Clone)]
pub struct RoutingStats {
    /// Total number of unique MAVLink systems currently known.
    pub total_systems: usize,
    /// Total number of specific `(system_id, component_id)` routes known.
    pub total_routes: usize,
    /// Total number of unique endpoint IDs represented in the routing table.
    pub total_endpoints: usize,
    /// Timestamp when these statistics were gathered (seconds since UNIX EPOCH).
    pub timestamp: u64,
}

impl RoutingTable {
    /// Creates a new, empty `RoutingTable`.
    pub fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| RwLock::new(Shard::default())),
            config: RwLock::new(Config::default()),
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

    /// Returns `true` if any endpoint in `endpoints` shares a group with
    /// `endpoint_id`. Requires a read lock on `config`.
    fn any_in_same_group(
        cfg: &Config,
        endpoints: &HashSet<EndpointId>,
        endpoint_id: EndpointId,
    ) -> bool {
        let Some(group) = cfg.endpoint_groups.get(&endpoint_id) else {
            return false;
        };
        let Some(members) = cfg.group_members.get(group) else {
            return false;
        };
        endpoints.iter().any(|ep| members.contains(ep))
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
                || Self::any_in_same_group(&cfg, &entry.endpoints, endpoint_id);
        }

        // Specific component route
        if let Some(comp_entry) = shard.routes.get(&(target_sysid, target_compid)) {
            return comp_entry.endpoints.contains(&endpoint_id)
                || Self::any_in_same_group(&cfg, &comp_entry.endpoints, endpoint_id);
        }

        // Aggressive fallback: we know the system but not this component.
        entry.endpoints.contains(&endpoint_id)
            || Self::any_in_same_group(&cfg, &entry.endpoints, endpoint_id)
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

/// TTL-prune one shard. Caller holds the shard's write lock.
fn shard_prune(shard: &mut Shard, max_age: Duration) {
    let now = Instant::now();
    let mut removed: HashMap<EndpointId, usize> = HashMap::new();

    shard.routes.retain(|_, entry| {
        let expired = now.duration_since(entry.last_seen) > max_age;
        if expired {
            for &ep_id in &entry.endpoints {
                let c = removed.entry(ep_id).or_insert(0);
                *c = c.saturating_add(1);
            }
        }
        !expired
    });

    shard.sys_routes.retain(|_, entry| {
        let expired = now.duration_since(entry.last_seen) > max_age;
        if expired {
            for &ep_id in &entry.endpoints {
                let c = removed.entry(ep_id).or_insert(0);
                *c = c.saturating_add(1);
            }
        }
        !expired
    });

    for (ep_id, count) in removed {
        if let Entry::Occupied(mut occ) = shard.endpoint_counts.entry(ep_id) {
            let current = occ.get_mut();
            *current = current.saturating_sub(count);
            if *occ.get() == 0 {
                occ.remove();
            }
        }
    }
}

/// Remove the oldest `sys_routes` entry in this shard (and all of its
/// associated component routes). Caller holds the shard's write lock.
fn shard_remove_oldest_system(shard: &mut Shard) {
    let oldest_entry = shard
        .sys_routes
        .iter()
        .min_by_key(|(_, entry)| entry.last_seen);

    let Some((&oldest_sysid, _)) = oldest_entry else {
        return;
    };

    if let Some(removed_entry) = shard.sys_routes.remove(&oldest_sysid) {
        for &ep_id in &removed_entry.endpoints {
            if let Entry::Occupied(mut occ) = shard.endpoint_counts.entry(ep_id) {
                let count = occ.get_mut();
                *count = count.saturating_sub(1);
                if *occ.get() == 0 {
                    occ.remove();
                }
            }
        }
    }
    shard.routes.retain(|(s, _), entry| {
        if *s == oldest_sysid {
            for &ep_id in &entry.endpoints {
                if let Entry::Occupied(mut occ) = shard.endpoint_counts.entry(ep_id) {
                    let count = occ.get_mut();
                    *count = count.saturating_sub(1);
                    if *occ.get() == 0 {
                        occ.remove();
                    }
                }
            }
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests;
