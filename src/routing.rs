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
    /// Logic:
    /// 1. If target_sysid == 0: Broadcast message, send to all
    /// 2. If we have a route for (target_sysid, target_compid): Send only to endpoints that have seen this combination
    /// 3. If we have a route for target_sysid but not the specific component: Send to all endpoints that have seen this system
    /// 4. If no route exists: Don't send (message would be dropped anyway)
    /// 
    /// Performance: O(1) HashMap lookup
    pub fn should_send(&self, endpoint_id: usize, target_sysid: u8, target_compid: u8) -> bool {
        // Broadcast messages go to everyone
        if target_sysid == 0 {
            return true;
        }

        // Check if we have seen this system at all
        if let Some(entry) = self.sys_routes.get(&target_sysid) {
            // If target_compid is 0 (system-wide), send to all endpoints that have this system
            if target_compid == 0 {
                return entry.endpoints.contains(&endpoint_id);
            }

            // Check for specific component route
            if let Some(comp_entry) = self.routes.get(&(target_sysid, target_compid)) {
                return comp_entry.endpoints.contains(&endpoint_id);
            }

            // We know the system but not this specific component
            // Send to all endpoints that have seen this system (component might be new)
            return entry.endpoints.contains(&endpoint_id);
        }

        // No route to this system - don't send
        false
    }

    pub fn prune(&mut self, max_age: Duration) {
        let now = Instant::now();
        // Prune component routes
        self.routes.retain(|_, v| now.duration_since(v.last_seen) < max_age);
        // Prune system routes
        self.sys_routes.retain(|_, v| now.duration_since(v.last_seen) < max_age);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_routing_table_basic_learning() {
        let mut rt = RoutingTable::new();

        // Endpoint 1 sees system 100, component 1
        rt.update(1, 100, 1);

        // Messages to sys 100 should go to endpoint 1
        assert!(rt.should_send(1, 100, 0), "System-wide message should route");
        assert!(rt.should_send(1, 100, 1), "Specific component should route");
        assert!(rt.should_send(1, 100, 2), "Unknown component on known system should route");

        // Messages to sys 100 should NOT go to endpoint 2
        assert!(!rt.should_send(2, 100, 0), "Endpoint 2 hasn't seen sys 100");
    }

    #[test]
    fn test_broadcast_always_sends() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);

        // Broadcast (target_system=0) goes to everyone
        assert!(rt.should_send(1, 0, 0));
        assert!(rt.should_send(2, 0, 0));
        assert!(rt.should_send(999, 0, 0));
    }

    #[test]
    fn test_unknown_system_no_route() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);

        // System 200 unknown - no route
        assert!(!rt.should_send(1, 200, 0));
        assert!(!rt.should_send(1, 200, 1));
        assert!(!rt.should_send(2, 200, 0));
    }

    #[test]
    fn test_multiple_endpoints_same_system() {
        let mut rt = RoutingTable::new();

        // Both endpoints see system 100
        rt.update(1, 100, 1);
        rt.update(2, 100, 1);

        // Both should get messages to sys 100
        assert!(rt.should_send(1, 100, 0));
        assert!(rt.should_send(2, 100, 0));
    }

    #[test]
    fn test_component_specific_routing() {
        let mut rt = RoutingTable::new();

        // Endpoint 1 sees sys 100 comp 1
        // Endpoint 2 sees sys 100 comp 2
        rt.update(1, 100, 1);
        rt.update(2, 100, 2);

        // System-wide messages go to both
        assert!(rt.should_send(1, 100, 0));
        assert!(rt.should_send(2, 100, 0));

        // Component-specific messages route correctly
        assert!(rt.should_send(1, 100, 1));
        assert!(rt.should_send(2, 100, 2));

        // If we have a specific route for (100, 2) -> Endpoint 2,
        // Endpoint 1 should NOT receive it (strict unicast).
        assert!(!rt.should_send(1, 100, 2), "Endpoint 1 has sys 100, but comp 2 is explicitly elsewhere");
        assert!(!rt.should_send(2, 100, 1), "Endpoint 2 has sys 100, but comp 1 is explicitly elsewhere");
        
        // Unknown component (100, 3) -> Broadcast to all who know 100
        assert!(rt.should_send(1, 100, 3));
        assert!(rt.should_send(2, 100, 3));
    }

    #[test]
    fn test_pruning() {
        let mut rt = RoutingTable::new();
        rt.update(1, 100, 1);

        assert!(rt.should_send(1, 100, 0));

        // Sleep longer than TTL
        std::thread::sleep(Duration::from_millis(50));

        // Prune with short TTL
        rt.prune(Duration::from_millis(10));

        // Route should be gone
        assert!(!rt.should_send(1, 100, 0), "Pruned route should not send");
    }
}
