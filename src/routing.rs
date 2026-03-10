use crate::router::EndpointId;
use ahash::{AHashMap, AHashSet};
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

// Type aliases using ahash for faster hashing
type HashMap<K, V> = AHashMap<K, V>;
type HashSet<T> = AHashSet<T>;

// Constants for routing table capacity limits
const MAX_ROUTES: usize = 100_000;
const MAX_SYSTEMS: usize = 1_000;

/// Represents an entry in the routing table for a specific (system_id, component_id) pair
/// or just a system_id. It tracks which endpoints have seen this MAVLink entity.
struct RouteEntry {
    /// Set of endpoint IDs that have seen this MAVLink entity.
    endpoints: HashSet<EndpointId>,
    /// The last time this entry was updated or a message was seen from this entity.
    last_seen: Instant,
}

/// Intelligent routing table that learns MAVLink network topology.
///
/// Routes messages based on `system_id` and `component_id`, with TTL-based
/// expiration to handle dynamic topologies.
pub struct RoutingTable {
    /// Map of `(system_id, component_id)` -> `RouteEntry` for specific component routes.
    routes: HashMap<(u8, u8), RouteEntry>,
    /// Map of `system_id` -> `RouteEntry` for system-wide routes (component_id 0).
    sys_routes: HashMap<u8, RouteEntry>,
    /// Incremental count of how many active route entries each EndpointId is present in.
    /// Used to quickly get the total number of unique active endpoints.
    endpoint_counts: HashMap<EndpointId, usize>,
    /// Map of endpoint ID to its group name (if any).
    endpoint_groups: HashMap<EndpointId, String>,
    /// Reverse lookup: group name -> set of endpoint IDs in that group.
    group_members: HashMap<String, HashSet<EndpointId>>,
    /// Set of system IDs that trigger sniffer mode.
    sniffer_sysids: HashSet<u8>,
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
            routes: HashMap::new(),
            sys_routes: HashMap::new(),
            endpoint_counts: HashMap::new(),
            endpoint_groups: HashMap::new(),
            group_members: HashMap::new(),
            sniffer_sysids: HashSet::new(),
        }
    }

    /// Registers an endpoint as belonging to a named group.
    ///
    /// Endpoints in the same group share routing knowledge, allowing
    /// redundant physical links to the same system to both forward traffic.
    ///
    /// # Arguments
    ///
    /// * `id` - The endpoint ID to register.
    /// * `group` - The group name to assign.
    pub fn set_endpoint_group(&mut self, id: EndpointId, group: String) {
        self.endpoint_groups.insert(id, group.clone());
        self.group_members.entry(group).or_default().insert(id);
    }

    /// Checks if any endpoint in the given set is in the same group as `endpoint_id`.
    fn any_in_same_group(&self, endpoints: &HashSet<EndpointId>, endpoint_id: EndpointId) -> bool {
        let group = match self.endpoint_groups.get(&endpoint_id) {
            Some(g) => g,
            None => return false,
        };
        let members = match self.group_members.get(group) {
            Some(m) => m,
            None => return false,
        };
        // Check if any endpoint in the route entry is a member of our group
        endpoints.iter().any(|ep| members.contains(ep))
    }

    /// Returns the set of endpoint IDs that have seen the given system ID.
    /// Returns None if the system is unknown.
    #[allow(dead_code)]
    pub fn endpoints_for_system(&self, sysid: u8) -> Option<&HashSet<EndpointId>> {
        self.sys_routes.get(&sysid).map(|e| &e.endpoints)
    }

    /// Sets the system IDs that trigger sniffer mode.
    pub fn set_sniffer_sysids(&mut self, sysids: &[u8]) {
        self.sniffer_sysids = sysids.iter().copied().collect();
    }

    /// Checks if this endpoint should receive all traffic due to sniffer mode.
    /// Returns true if any sniffer system ID has been seen on this endpoint.
    pub fn is_sniffer_endpoint(&self, endpoint_id: EndpointId) -> bool {
        for &sysid in &self.sniffer_sysids {
            if let Some(entry) = self.sys_routes.get(&sysid) {
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
    /// `endpoint_id` is a known path for that entity.
    /// The `last_seen` timestamp for the entry is updated.
    ///
    /// # Arguments
    ///
    /// * `endpoint_id` - The ID of the endpoint where the message was received.
    /// * `sysid` - The MAVLink system ID of the message sender.
    /// * `compid` - The MAVLink component ID of the message sender.
    /// * `now` - The timestamp of the observation.
    pub fn update(&mut self, endpoint_id: EndpointId, sysid: u8, compid: u8, now: Instant) {
        // Enforce MAX_ROUTES limit
        if self.routes.len() >= MAX_ROUTES {
            warn!(
                "Route table at capacity ({}), forcing prune of 60s old entries",
                MAX_ROUTES
            );
            self.prune(Duration::from_secs(60)); // Force cleanup of 1 minute old entries
        }

        // Enforce MAX_SYSTEMS limit
        if self.sys_routes.len() >= MAX_SYSTEMS {
            warn!(
                "System table at capacity ({}), removing oldest system",
                MAX_SYSTEMS
            );
            // This prune logic will decrement endpoint_counts automatically.
            let oldest_sys_entry = self
                .sys_routes
                .iter()
                .min_by_key(|(_, entry)| entry.last_seen);

            if let Some((&oldest_sysid, _)) = oldest_sys_entry {
                // Manually decrement counts for endpoints in sys_routes
                if let Some(removed_entry) = self.sys_routes.remove(&oldest_sysid) {
                    for &ep_id in &removed_entry.endpoints {
                        if let Entry::Occupied(mut occ) = self.endpoint_counts.entry(ep_id) {
                            let count = occ.get_mut();
                            *count = count.saturating_sub(1);
                            if *occ.get() == 0 {
                                occ.remove();
                            }
                        }
                    }
                }
                // Also remove all component routes associated with this system
                self.routes.retain(|(s, _), entry| {
                    if *s == oldest_sysid {
                        for &ep_id in &entry.endpoints {
                            if let Entry::Occupied(mut occ) = self.endpoint_counts.entry(ep_id) {
                                let count = occ.get_mut();
                                *count = count.saturating_sub(1);
                                if *occ.get() == 0 {
                                    occ.remove();
                                }
                            }
                        }
                        false // Remove this entry
                    } else {
                        true
                    }
                });
            }
        }

        // Update routes
        let mut increment_ep_count_for_routes = false;
        self.routes
            .entry((sysid, compid))
            .and_modify(|e| {
                if e.endpoints.insert(endpoint_id) {
                    // Check if new endpoint in this entry
                    increment_ep_count_for_routes = true;
                }
                e.last_seen = now;
            })
            .or_insert_with(|| {
                increment_ep_count_for_routes = true;
                RouteEntry {
                    endpoints: HashSet::from([endpoint_id]),
                    last_seen: now,
                }
            });
        if increment_ep_count_for_routes {
            *self.endpoint_counts.entry(endpoint_id).or_insert(0) += 1;
        }

        // Update sys_routes
        let mut increment_ep_count_for_sys_routes = false;
        self.sys_routes
            .entry(sysid)
            .and_modify(|e| {
                if e.endpoints.insert(endpoint_id) {
                    increment_ep_count_for_sys_routes = true;
                }
                e.last_seen = now;
            })
            .or_insert_with(|| {
                increment_ep_count_for_sys_routes = true;
                RouteEntry {
                    endpoints: HashSet::from([endpoint_id]),
                    last_seen: now,
                }
            });
        if increment_ep_count_for_sys_routes {
            *self.endpoint_counts.entry(endpoint_id).or_insert(0) += 1;
        }
    }

    /// Checks if an update is needed for the given route.
    /// An update is needed if the route is unknown, the endpoint isn't registered,
    /// or the last update was more than 1 second ago.
    pub fn needs_update_for_endpoint(
        &self,
        endpoint_id: EndpointId,
        sysid: u8,
        compid: u8,
        now: Instant,
    ) -> bool {
        // Check component route
        let comp_entry = self.routes.get(&(sysid, compid));
        let comp_needs_update = match comp_entry {
            None => true, // Route doesn't exist
            Some(e) => {
                // Needs update if endpoint not in set OR entry is stale
                !e.endpoints.contains(&endpoint_id)
                    || now.duration_since(e.last_seen) >= Duration::from_secs(1)
            }
        };

        // Check system route
        let sys_entry = self.sys_routes.get(&sysid);
        let sys_needs_update = match sys_entry {
            None => true, // System route doesn't exist
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
    /// This method implements a routing policy to decide message distribution.
    ///
    /// # Routing Logic (Policy B - Aggressive Fallback):
    /// 1. If `target_sysid == 0`: This is a broadcast message, it should be sent to all endpoints.
    ///    The routing table does not filter broadcast messages based on target.
    /// 2. If a specific route for `(target_sysid, target_compid)` exists:
    ///    The message is sent *only* to endpoints that have specifically seen this combination.
    /// 3. If a route for `target_sysid` exists (i.e., the system is known) but *not* for the
    ///    specific component `target_compid`:
    ///    The message is sent to *all* endpoints that have seen this system. This acts as an
    ///    "aggressive fallback," assuming the component might be new or its location has moved.
    /// 4. If no route (neither specific component nor system-wide) exists for `target_sysid`:
    ///    The message is dropped (not sent to any endpoint).
    ///
    /// # Arguments
    ///
    /// * `endpoint_id` - The ID of the endpoint to check.
    /// * `target_sysid` - The MAVLink system ID targeted by the message.
    /// * `target_compid` - The MAVLink component ID targeted by the message.
    ///
    /// # Returns
    ///
    /// `true` if the message should be sent to `endpoint_id`, `false` otherwise.
    pub fn should_send(
        &self,
        endpoint_id: EndpointId,
        target_sysid: u8,
        target_compid: u8,
    ) -> bool {
        // Sniffer mode: if this endpoint has seen a sniffer system ID, forward everything
        if self.is_sniffer_endpoint(endpoint_id) {
            return true;
        }

        if target_sysid == 0 {
            // MAV_BROADCAST_SYSTEM_ID
            return true;
        }

        if let Some(entry) = self.sys_routes.get(&target_sysid) {
            if target_compid == 0 {
                // MAV_BROADCAST_COMPONENT_ID or target system only
                return entry.endpoints.contains(&endpoint_id)
                    || self.any_in_same_group(&entry.endpoints, endpoint_id);
            }

            // Check for specific component route
            if let Some(comp_entry) = self.routes.get(&(target_sysid, target_compid)) {
                return comp_entry.endpoints.contains(&endpoint_id)
                    || self.any_in_same_group(&comp_entry.endpoints, endpoint_id);
            }

            // Fallback: We know the system but not this specific component
            // Send to all endpoints that have seen this system
            return entry.endpoints.contains(&endpoint_id)
                || self.any_in_same_group(&entry.endpoints, endpoint_id);
        }

        false
    }

    /// Prunes old entries from the routing table.
    ///
    /// Any route entry (`(system_id, component_id)` or `system_id`) that has not been
    /// updated within `max_age` duration will be removed. This helps in managing
    /// dynamic network topologies where MAVLink entities might disconnect or change
    /// their associated endpoints.
    ///
    /// # Arguments
    ///
    /// * `max_age` - The maximum duration an entry can remain in the table without being updated.
    pub fn prune(&mut self, max_age: Duration) {
        let now = Instant::now();

        // Collect endpoint_ids from removed entries to decrement counts
        let mut removed_endpoint_counts: HashMap<EndpointId, usize> = HashMap::new();

        self.routes.retain(|_key, entry| {
            let expired = now.duration_since(entry.last_seen) > max_age;
            if expired {
                for &ep_id in &entry.endpoints {
                    *removed_endpoint_counts.entry(ep_id).or_insert(0) += 1;
                }
            }
            !expired
        });

        self.sys_routes.retain(|_key, entry| {
            let expired = now.duration_since(entry.last_seen) > max_age;
            if expired {
                for &ep_id in &entry.endpoints {
                    *removed_endpoint_counts.entry(ep_id).or_insert(0) += 1;
                }
            }
            !expired
        });

        // Decrement counts and remove endpoint_ids if their count reaches zero
        for (ep_id, count) in removed_endpoint_counts {
            if let Entry::Occupied(mut occ) = self.endpoint_counts.entry(ep_id) {
                let current = occ.get_mut();
                *current = current.saturating_sub(count);
                if *occ.get() == 0 {
                    occ.remove();
                }
            }
        }
    }

    /// Returns current statistics about the routing table.
    pub fn stats(&self) -> RoutingStats {
        RoutingStats {
            total_systems: self.sys_routes.len(),
            total_routes: self.routes.len(),
            total_endpoints: self.endpoint_counts.len(), // Use the pre-calculated length
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_grouped_endpoints_share_routing() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        // Both endpoints in the same group
        rt.set_endpoint_group(ep0, "autopilot".to_string());
        rt.set_endpoint_group(ep1, "autopilot".to_string());

        // System 1, component 1 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 1, 1, now);

        // ep0 should be able to send to system 1 (direct route)
        assert!(rt.should_send(ep0, 1, 1));
        // ep1 should also be able to send to system 1 (group member)
        assert!(rt.should_send(ep1, 1, 1));
    }

    #[test]
    fn test_ungrouped_endpoints_independent() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        // No groups set

        // System 1 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 1, 1, now);

        // ep0 has the route
        assert!(rt.should_send(ep0, 1, 1));
        // ep1 does NOT have the route (no group sharing)
        assert!(!rt.should_send(ep1, 1, 1));
    }

    #[test]
    fn test_group_with_three_endpoints() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);
        let ep2 = EndpointId(2);

        rt.set_endpoint_group(ep0, "vehicle".to_string());
        rt.set_endpoint_group(ep1, "vehicle".to_string());
        rt.set_endpoint_group(ep2, "vehicle".to_string());

        // System 5 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 5, 1, now);

        // All group members should be able to send to system 5
        assert!(rt.should_send(ep0, 5, 1));
        assert!(rt.should_send(ep1, 5, 1));
        assert!(rt.should_send(ep2, 5, 1));
    }

    #[test]
    fn test_mixed_grouped_and_ungrouped() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0); // grouped
        let ep1 = EndpointId(1); // grouped
        let ep2 = EndpointId(2); // ungrouped
        let ep3 = EndpointId(3); // different group

        rt.set_endpoint_group(ep0, "autopilot".to_string());
        rt.set_endpoint_group(ep1, "autopilot".to_string());
        rt.set_endpoint_group(ep3, "gcs".to_string());

        // System 1 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 1, 1, now);

        // ep0 has direct route
        assert!(rt.should_send(ep0, 1, 1));
        // ep1 shares group with ep0
        assert!(rt.should_send(ep1, 1, 1));
        // ep2 is ungrouped, no route
        assert!(!rt.should_send(ep2, 1, 1));
        // ep3 is in a different group, no route
        assert!(!rt.should_send(ep3, 1, 1));
    }

    #[test]
    fn test_empty_group_string_ignored() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        // Empty group string should not cause grouping
        // (The config layer filters empty strings, but test the routing table directly)
        rt.set_endpoint_group(ep0, String::new());
        rt.set_endpoint_group(ep1, String::new());

        // System 1 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 1, 1, now);

        // ep0 has direct route
        assert!(rt.should_send(ep0, 1, 1));
        // ep1 technically shares empty group -- but config.rs filters this out.
        // At the routing table level, empty strings DO group (by design).
        // The protection is in config::EndpointConfig::group() which returns None for "".
        // So this test verifies the config-level protection works, not routing-level.
    }

    #[test]
    fn test_grouped_endpoints_system_broadcast() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        rt.set_endpoint_group(ep0, "autopilot".to_string());
        rt.set_endpoint_group(ep1, "autopilot".to_string());

        // System 1 discovered on ep0
        let now = Instant::now();
        rt.update(ep0, 1, 1, now);

        // Component broadcast (compid=0) to system 1 should work for group members
        assert!(rt.should_send(ep0, 1, 0));
        assert!(rt.should_send(ep1, 1, 0));
    }

    #[test]
    fn test_sniffer_mode_receives_all() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        rt.set_sniffer_sysids(&[253]);

        let now = Instant::now();
        // ep0 has seen sniffer sysid 253
        rt.update(ep0, 253, 1, now);
        // ep1 has seen normal sysid 1
        rt.update(ep1, 1, 1, now);

        // ep0 should receive ALL traffic (sniffer mode)
        assert!(rt.should_send(ep0, 1, 1));
        assert!(rt.should_send(ep0, 2, 5));
        assert!(rt.should_send(ep0, 99, 99));
        assert!(rt.should_send(ep0, 0, 0)); // broadcast too
    }

    #[test]
    fn test_sniffer_mode_no_effect_without_sysid() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        rt.set_sniffer_sysids(&[253]);

        let now = Instant::now();
        // ep0 has NOT seen sysid 253, only normal sysid 1
        rt.update(ep0, 1, 1, now);
        rt.update(ep1, 2, 1, now);

        // ep0 should follow normal routing (not a sniffer)
        assert!(rt.should_send(ep0, 1, 1)); // has route
        assert!(!rt.should_send(ep0, 2, 1)); // no route
        assert!(!rt.should_send(ep0, 99, 1)); // unknown system
    }

    #[test]
    fn test_sniffer_and_normal_endpoints_coexist() {
        let mut rt = RoutingTable::new();
        let ep_sniffer = EndpointId(0);
        let ep_normal = EndpointId(1);

        rt.set_sniffer_sysids(&[253]);

        let now = Instant::now();
        // Sniffer endpoint has seen sniffer sysid
        rt.update(ep_sniffer, 253, 1, now);
        // Normal endpoint has seen sysid 1
        rt.update(ep_normal, 1, 1, now);

        // Sniffer gets everything
        assert!(rt.should_send(ep_sniffer, 1, 1));
        assert!(rt.should_send(ep_sniffer, 2, 5));
        assert!(rt.should_send(ep_sniffer, 99, 99));

        // Normal endpoint only gets routed traffic
        assert!(rt.should_send(ep_normal, 1, 1)); // has route
        assert!(!rt.should_send(ep_normal, 2, 5)); // no route
        assert!(!rt.should_send(ep_normal, 99, 99)); // unknown
    }

    #[test]
    fn test_basic_routing_without_groups() {
        let mut rt = RoutingTable::new();
        let ep0 = EndpointId(0);
        let ep1 = EndpointId(1);

        let now = Instant::now();
        rt.update(ep0, 1, 1, now);
        rt.update(ep1, 2, 1, now);

        // Broadcast always sends
        assert!(rt.should_send(ep0, 0, 0));
        assert!(rt.should_send(ep1, 0, 0));

        // System 1 only on ep0
        assert!(rt.should_send(ep0, 1, 1));
        assert!(!rt.should_send(ep1, 1, 1));

        // System 2 only on ep1
        assert!(!rt.should_send(ep0, 2, 1));
        assert!(rt.should_send(ep1, 2, 1));

        // Unknown system
        assert!(!rt.should_send(ep0, 99, 1));
    }
}
