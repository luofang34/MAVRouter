use std::collections::{HashMap, HashSet};
use std::time::{Instant, Duration};

struct RouteEntry {
    endpoints: HashSet<usize>,
    last_seen: Instant,
}

pub struct RoutingTable {
    // Map of (sysid, compid) -> RouteEntry
    routes: HashMap<(u8, u8), RouteEntry>,
    // Map of sysid -> RouteEntry (for system-wide routing)
    sys_routes: HashMap<u8, RouteEntry>,
}

impl Default for RoutingTable {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RoutingStats {
    pub total_systems: usize,
    pub total_routes: usize,
    pub total_endpoints: usize,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
            sys_routes: HashMap::new(),
        }
    }

    pub fn update(&mut self, endpoint_id: usize, sysid: u8, compid: u8) {
        let now = Instant::now();
        
        self.routes.entry((sysid, compid))
            .and_modify(|e| {
                e.endpoints.insert(endpoint_id);
                e.last_seen = now;
            })
            .or_insert_with(|| RouteEntry {
                endpoints: HashSet::from([endpoint_id]),
                last_seen: now,
            });
            
        self.sys_routes.entry(sysid)
            .and_modify(|e| {
                e.endpoints.insert(endpoint_id);
                e.last_seen = now;
            })
            .or_insert_with(|| RouteEntry {
                endpoints: HashSet::from([endpoint_id]),
                last_seen: now,
            });
    }

    /// Check if a message should be sent to a specific endpoint based on learned routes
    /// 
    /// Logic (Policy B - Aggressive Fallback):
    /// 1. If target_sysid == 0: Broadcast message, send to all
    /// 2. If we have a specific route for (target_sysid, target_compid): Send ONLY to endpoints that have seen this combination.
    /// 3. If we have a route for target_sysid but NOT the specific component: Send to ALL endpoints that have seen this system (assuming component might be new/moved).
    /// 4. If no route exists: Don't send.
    pub fn should_send(&self, endpoint_id: usize, target_sysid: u8, target_compid: u8) -> bool {
        if target_sysid == 0 {
            return true;
        }

        if let Some(entry) = self.sys_routes.get(&target_sysid) {
            if target_compid == 0 {
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

    pub fn prune(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.routes.retain(|_, v| now.duration_since(v.last_seen) < max_age);
        self.sys_routes.retain(|_, v| now.duration_since(v.last_seen) < max_age);
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> RoutingStats {
        RoutingStats {
            total_systems: self.sys_routes.len(),
            total_routes: self.routes.len(),
            total_endpoints: self.sys_routes.values()
                .flat_map(|e| e.endpoints.iter())
                .collect::<HashSet<_>>()
                .len(),
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
        assert!(!rt.should_send(2, 100, 1), "Strict routing for known component");

        assert!(rt.should_send(2, 100, 2));
        assert!(!rt.should_send(1, 100, 2), "Strict routing for known component");

        // Unknown component (100, 3) on known system: FALLBACK
        // Both endpoints know system 100.
        assert!(rt.should_send(1, 100, 3), "New component, fallback to sys route");
        assert!(rt.should_send(2, 100, 3), "New component, fallback to sys route");
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
