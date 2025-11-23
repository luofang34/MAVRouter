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

    #[allow(dead_code)]
    pub fn should_send(&self, endpoint_id: usize, target_sysid: u8, target_compid: u8) -> bool {
        if target_sysid == 0 {
            return true;
        }

        if let Some(entry) = self.sys_routes.get(&target_sysid) {
            if target_compid == 0 {
                return entry.endpoints.contains(&endpoint_id);
            }
            
            if let Some(comp_entry) = self.routes.get(&(target_sysid, target_compid)) {
                return comp_entry.endpoints.contains(&endpoint_id);
            } else {
                 return entry.endpoints.contains(&endpoint_id);
            }
        }

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
