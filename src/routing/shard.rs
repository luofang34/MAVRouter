//! Per-`system_id` shard of the routing table.
//!
//! Each [`Shard`] owns its own routes, sys-routes, and per-endpoint count
//! map; sharding by `system_id & SHARD_MASK` lets read traffic on one
//! system avoid serialising behind a write for an unrelated system.
//!
//! The capacity-enforcement and TTL-prune helpers ([`shard_prune`],
//! [`shard_remove_oldest_system`]) all assume the caller already holds the
//! shard's write lock.

use crate::router::EndpointId;
use ahash::{AHashMap, AHashSet};
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant};

/// Shorter aliases over `ahash`'s faster non-cryptographic maps/sets.
pub(super) type HashMap<K, V> = AHashMap<K, V>;
/// Shorter alias over `ahash`'s faster non-cryptographic set.
pub(super) type HashSet<T> = AHashSet<T>;

/// Number of shards the routing table is partitioned into, keyed by
/// `system_id & SHARD_MASK`.
///
/// 16 shards is the sweet spot on the current workloads: under concurrent
/// read+write load, reads for `sys_id=5` don't stall behind a write for
/// `sys_id=7`, while the per-shard memory overhead stays negligible.
/// `system_id` is a `u8`, so 16 shards puts each shard responsible for
/// 16 ids on average — well distributed even for small installations.
pub(super) const SHARD_COUNT: usize = 16;
/// Bit-mask matching [`SHARD_COUNT`]; used to index a `system_id` into a shard.
pub(super) const SHARD_MASK: usize = SHARD_COUNT - 1;

/// Per-shard cap on `routes` entries. The original global cap (100k) is
/// divided by [`SHARD_COUNT`] so the total addressable capacity stays the
/// same as the pre-sharded design.
pub(super) const MAX_ROUTES_PER_SHARD: usize = 100_000 / SHARD_COUNT;
/// Per-shard cap on `sys_routes` entries. Derived from the original global
/// cap (1k) divided by [`SHARD_COUNT`].
pub(super) const MAX_SYSTEMS_PER_SHARD: usize = 1_000 / SHARD_COUNT;

/// Represents an entry in the routing table for a specific (system_id, component_id) pair
/// or just a system_id. It tracks which endpoints have seen this MAVLink entity.
pub(super) struct RouteEntry {
    /// Set of endpoint IDs that have seen this MAVLink entity.
    pub(super) endpoints: HashSet<EndpointId>,
    /// The last time this entry was updated or a message was seen from this entity.
    pub(super) last_seen: Instant,
}

/// Per-shard state: only the routes whose `system_id` hashes to this shard.
/// Each shard holds an independent `RwLock`, so read traffic on different
/// sysids doesn't serialise through a single writer.
#[derive(Default)]
pub(super) struct Shard {
    /// Map of `(system_id, component_id)` -> `RouteEntry` for specific component routes.
    /// All keys here satisfy `system_id as usize & SHARD_MASK == <this shard index>`.
    pub(super) routes: HashMap<(u8, u8), RouteEntry>,
    /// Map of `system_id` -> `RouteEntry` for system-wide routes (component_id 0).
    pub(super) sys_routes: HashMap<u8, RouteEntry>,
    /// Incremental count of how many active route entries each EndpointId is
    /// present in *within this shard*. The global count (used by stats) is
    /// the set-union of keys across all shards, not the sum of values.
    pub(super) endpoint_counts: HashMap<EndpointId, usize>,
}

/// TTL-prune one shard. Caller holds the shard's write lock.
pub(super) fn shard_prune(shard: &mut Shard, max_age: Duration) {
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
pub(super) fn shard_remove_oldest_system(shard: &mut Shard) {
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
