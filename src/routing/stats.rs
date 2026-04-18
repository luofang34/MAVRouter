//! `RoutingStats`: a point-in-time snapshot of routing-table cardinality,
//! returned by [`super::table::RoutingTable::stats`] for the metrics
//! pipeline (stats reporter + Unix-socket query handler).

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
