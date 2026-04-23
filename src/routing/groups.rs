//! Endpoint-group and sniffer-sysid configuration for the routing table.
//!
//! Renamed from the earlier `Config` (which collided with the top-level
//! [`crate::config::Config`]); now [`GroupConfig`] makes the intent —
//! "routing-side group / sniffer policy" — explicit at the use site.
//!
//! Held behind [`arc_swap::ArcSwap`] in [`super::table::RoutingTable`] so
//! hot-path reads are a lock-free atomic pointer load. Writes are rare
//! (called at startup via `set_endpoint_group` / `set_sniffer_sysids`)
//! and use copy-on-write: load the current snapshot, clone, mutate,
//! publish — hence the `Clone` derive here is load-bearing.

use super::shard::{HashMap, HashSet};
use crate::router::EndpointId;

/// Group / sniffer configuration. Written rarely (startup), read on the
/// hot path. Kept Clone-able so [`super::table::RoutingTable`] can
/// copy-on-write through `ArcSwap` without ever blocking a reader.
#[derive(Default, Clone)]
pub(super) struct GroupConfig {
    /// Map of endpoint ID to its group name (if any).
    pub(super) endpoint_groups: HashMap<EndpointId, String>,
    /// Reverse lookup: group name -> set of endpoint IDs in that group.
    pub(super) group_members: HashMap<String, HashSet<EndpointId>>,
    /// Set of system IDs that trigger sniffer mode.
    pub(super) sniffer_sysids: HashSet<u8>,
}

impl GroupConfig {
    /// Returns `true` if any endpoint in `endpoints` shares a group with
    /// `endpoint_id`.
    pub(super) fn any_in_same_group(
        &self,
        endpoints: &HashSet<EndpointId>,
        endpoint_id: EndpointId,
    ) -> bool {
        let Some(group) = self.endpoint_groups.get(&endpoint_id) else {
            return false;
        };
        let Some(members) = self.group_members.get(group) else {
            return false;
        };
        endpoints.iter().any(|ep| members.contains(ep))
    }
}
