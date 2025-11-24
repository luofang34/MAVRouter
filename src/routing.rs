use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Represents an entry in the routing table for a specific (system_id, component_id) pair
/// or just a system_id. It tracks which endpoints have seen this MAVLink entity.
struct RouteEntry {
    /// Set of endpoint IDs that have seen this MAVLink entity.
    endpoints: HashSet<usize>,
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
        }
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
    pub fn update(&mut self, endpoint_id: usize, sysid: u8, compid: u8) {
        let now = Instant::now();

        self.routes
            .entry((sysid, compid))
            .and_modify(|e| {
                e.endpoints.insert(endpoint_id);
                e.last_seen = now;
            })
            .or_insert_with(|| RouteEntry {
                endpoints: HashSet::from([endpoint_id]),
                last_seen: now,
            });

        self.sys_routes
            .entry(sysid)
            .and_modify(|e| {
                e.endpoints.insert(endpoint_id);
                e.last_seen = now;
            })
            .or_insert_with(|| RouteEntry {
                endpoints: HashSet::from([endpoint_id]),
                last_seen: now,
            });
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
    pub fn should_send(&self, endpoint_id: usize, target_sysid: u8, target_compid: u8) -> bool {
        if target_sysid == 0 {
            // MAV_BROADCAST_SYSTEM_ID
            return true;
        }

        if let Some(entry) = self.sys_routes.get(&target_sysid) {
            if target_compid == 0 {
                // MAV_BROADCAST_COMPONENT_ID or target system only
                return entry.endpoints.contains(&endpoint_id);
            }

            // Check for specific component route
            if let Some(comp_entry) = self.routes.get(&(target_sysid, target_compid)) {
                return comp_entry.endpoints.contains(&endpoint_id);
            }

            // Fallback: We know the system but not this specific component
            // Send to all endpoints that have seen this system
            return entry.endpoints.contains(&endpoint_id);
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
        self.routes
            .retain(|_, v| now.duration_since(v.last_seen) < max_age);
        self.sys_routes
            .retain(|_, v| now.duration_since(v.last_seen) < max_age);
    }

    /// Returns current statistics about the routing table.
    pub fn stats(&self) -> RoutingStats {
        RoutingStats {
            total_systems: self.sys_routes.len(),
            total_routes: self.routes.len(),
            total_endpoints: self
                .sys_routes
                .values()
                .flat_map(|e| e.endpoints.iter())
                .collect::<HashSet<_>>()
                .len(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_routing_table_basic_learning() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);

        assert!(rt.should_send(1, 100, 0));
        assert!(rt.should_send(1, 100, 1));
        // Unknown component on known system -> Fallback (True)
        assert!(rt.should_send(1, 100, 2));
        assert!(!rt.should_send(2, 100, 0));
    }

    #[test]
    fn test_component_specific_routing_strict() {
        let mut rt = RoutingTable::new();

        // Endpoint 1 sees sys 100 comp 1
        // Endpoint 2 sees sys 100 comp 2
        rt.update(1, 100, 1);
        rt.update(2, 100, 2);

        // System-wide (comp=0) goes to both
        assert!(rt.should_send(1, 100, 0));
        assert!(rt.should_send(2, 100, 0));

        // Component-specific routes to exact match
        // E.g. (100, 1) is known on Endpoint 1. Should ONLY go to 1?
        // Yes, because `routes.get` succeeds.
        assert!(rt.should_send(1, 100, 1));
        // Does Endpoint 2 receive (100, 1)?
        // `routes.get(100, 1)` exists -> {1}.
        // `comp_entry.endpoints.contains(2)` -> False.
        assert!(
            !rt.should_send(2, 100, 1),
            "Strict routing for known component"
        );

        assert!(rt.should_send(2, 100, 2));
        assert!(
            !rt.should_send(1, 100, 2),
            "Strict routing for known component"
        );

        // Unknown component (100, 3) on known system: FALLBACK
        // Both endpoints know system 100.
        assert!(
            rt.should_send(1, 100, 3),
            "New component, fallback to sys route"
        );
        assert!(
            rt.should_send(2, 100, 3),
            "New component, fallback to sys route"
        );
    }

    #[test]
    fn test_component_isolation() {
        let mut rt = RoutingTable::new();

        // Endpoint 1: autopilot on sys 100
        // Endpoint 2: autopilot on sys 200
        rt.update(1, 100, 1);
        rt.update(2, 200, 1);

        // Message to sys 100 should ONLY go to endpoint 1
        assert!(rt.should_send(1, 100, 0));
        assert!(!rt.should_send(2, 100, 0));

        // Message to sys 200 should ONLY go to endpoint 2
        assert!(!rt.should_send(1, 200, 0));
        assert!(rt.should_send(2, 200, 0));
    }

    #[test]
    fn test_broadcast_always_sends() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);
        assert!(rt.should_send(1, 0, 0));
        assert!(rt.should_send(999, 0, 0));
    }

    #[test]
    fn test_unknown_system_no_route() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);
        assert!(!rt.should_send(1, 200, 0));
    }

    #[test]
    fn test_pruning() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);
        assert!(rt.should_send(1, 100, 0));
        std::thread::sleep(Duration::from_millis(50));
        rt.prune(Duration::from_millis(10));
        assert!(!rt.should_send(1, 100, 0));
    }
}
