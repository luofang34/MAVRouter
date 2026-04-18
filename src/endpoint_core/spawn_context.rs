//! [`EndpointSpawnContext`]: the shared bag of resources every transport's
//! `run()` needs.
//!
//! Before this lived as a struct, every TCP / UDP / Serial `run()` carried
//! 11–13 individually-named parameters and `orchestration::spawn_all` had to
//! re-spell the same names at every call site. Both directions cost the
//! same — adding one new dependency meant editing four function signatures
//! and one orchestration block. Bundling the common fields here keeps the
//! transport-specific signatures honest: `tcp::run(ctx, address, mode)`,
//! `udp::run(ctx, address, mode, cleanup_ttl, broadcast_timeout)`, etc.
//!
//! `EndpointSpawnContext` is `Clone` because the supervisor restarts a
//! transport's `run()` on every retry; the spawning closure captures one
//! `EndpointSpawnContext` and clones it per attempt.

use super::{EndpointCore, EndpointStats};
use crate::dedup::ConcurrentDedup;
use crate::filter::EndpointFilters;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::{RouteUpdate, RoutingTable};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Shared resources every endpoint's `run()` needs. Bundled into a single
/// struct so transport-specific `run()` signatures only carry the
/// transport-specific parameters (address, baud rate, etc.) on top of `ctx`.
///
/// Cheap to clone — every field is either `Clone`-by-value, an `Arc`, a
/// channel sender (refcounted), or a [`CancellationToken`] (also refcounted).
#[derive(Clone)]
pub struct EndpointSpawnContext {
    /// Endpoint id (passed to [`EndpointCore`] and used in log lines).
    pub id: usize,
    /// Sender half of the global message bus.
    pub bus_tx: broadcast::Sender<Arc<RoutedMessage>>,
    /// Shared routing table (read-only on the hot path).
    pub routing_table: Arc<RoutingTable>,
    /// Channel to the routing-updater task — endpoints submit
    /// [`RouteUpdate`]s here with `try_send`.
    pub route_update_tx: mpsc::Sender<RouteUpdate>,
    /// Shared deduplication instance.
    pub dedup: ConcurrentDedup,
    /// Per-endpoint filters.
    pub filters: EndpointFilters,
    /// Per-endpoint traffic counters.
    pub stats: Arc<EndpointStats>,
    /// Cancellation token signalling graceful shutdown.
    pub cancel_token: CancellationToken,
}

impl EndpointSpawnContext {
    /// Build the [`EndpointCore`] that the hot path uses.
    ///
    /// `update_routing` is the only field the context can't supply itself —
    /// TCP server's per-client cores set it to `true` (each client is a
    /// distinct route source) while spawned endpoints normally pass `true`
    /// as well. It stays an explicit argument so the policy is visible at
    /// the call site.
    pub fn endpoint_core(&self, update_routing: bool) -> EndpointCore {
        EndpointCore {
            id: EndpointId(self.id),
            bus_tx: self.bus_tx.clone(),
            routing_table: self.routing_table.clone(),
            route_update_tx: self.route_update_tx.clone(),
            dedup: self.dedup.clone(),
            filters: self.filters.clone(),
            update_routing,
            stats: self.stats.clone(),
        }
    }

    /// Subscribe a fresh receiver onto the message bus. Each call returns
    /// an independent `broadcast::Receiver` — supervisors call this once
    /// per restart so each transport run gets a fresh, lag-free receiver.
    pub fn subscribe_bus(&self) -> broadcast::Receiver<Arc<RoutedMessage>> {
        self.bus_tx.subscribe()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::dedup::ConcurrentDedup;
    use crate::filter::EndpointFilters;
    use crate::router::create_bus;
    use crate::routing::RoutingTable;
    use std::time::Duration;

    fn make_ctx(id: usize) -> EndpointSpawnContext {
        let bus = create_bus(16);
        let (route_tx, _route_rx) = mpsc::channel(8);
        EndpointSpawnContext {
            id,
            bus_tx: bus.sender(),
            routing_table: Arc::new(RoutingTable::new()),
            route_update_tx: route_tx,
            dedup: ConcurrentDedup::new(Duration::from_millis(100)),
            filters: EndpointFilters::default(),
            stats: Arc::new(EndpointStats::new()),
            cancel_token: CancellationToken::new(),
        }
    }

    #[test]
    fn endpoint_core_inherits_id_and_routing_flag() {
        let ctx = make_ctx(7);
        let core = ctx.endpoint_core(true);
        assert_eq!(core.id, EndpointId(7));
        assert!(core.update_routing);

        let core_no_route = ctx.endpoint_core(false);
        assert!(!core_no_route.update_routing);
    }

    #[test]
    fn subscribe_bus_returns_independent_receivers() {
        let ctx = make_ctx(0);
        // Each subscribe creates a fresh anchored cursor — the broadcast
        // sender's receiver_count tracks live subscriptions, so the count
        // grows by one per call. This is the property orchestration relies
        // on for supervisor restarts to start lag-free.
        let starting = ctx.bus_tx.receiver_count();
        let _r1 = ctx.subscribe_bus();
        let _r2 = ctx.subscribe_bus();
        assert_eq!(ctx.bus_tx.receiver_count(), starting + 2);
    }

    #[test]
    fn cloning_ctx_shares_underlying_resources() {
        // Supervisors clone the ctx every restart; cloned ctxs must share
        // routing table, bus, dedup, and stats so all retries observe the
        // same state. Verify Arc-based sharing for the routing table.
        let ctx1 = make_ctx(0);
        let ctx2 = ctx1.clone();
        assert!(Arc::ptr_eq(&ctx1.routing_table, &ctx2.routing_table));
        assert!(Arc::ptr_eq(&ctx1.stats, &ctx2.stats));
    }
}
