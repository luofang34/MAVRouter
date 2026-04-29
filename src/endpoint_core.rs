//! Core logic for MAVLink endpoints.
//!
//! This module defines the shared operational logic for various MAVLink endpoint types
//! (e.g., TCP, UDP, Serial). It includes the `EndpointCore` struct, which encapsulates
//! common resources and filtering mechanisms, and the `run_stream_loop` function,
//! responsible for processing MAVLink messages over asynchronous read/write streams.

mod backoff;
mod stats;

pub use backoff::ExponentialBackoff;
// `EndpointStatsSnapshot` is re-exported from `lib.rs` for downstream consumers
// of the public API; the bin compilation never names it directly, so its
// re-export trips the bin's unused-imports lint without the allow.
#[allow(unused_imports)]
pub use stats::{EndpointStats, EndpointStatsSnapshot};

use crate::dedup::ConcurrentDedup;
use crate::filter::EndpointFilters;
use crate::framing::MavlinkFrame;
use crate::mavlink_utils::extract_target;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::{RouteUpdate, RoutingTable};
use mavlink::{MavHeader, Message};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast::{self};
use tokio::sync::mpsc;
use tracing::{trace, warn};

/// Represents the shared core logic and resources for a MAVLink endpoint.
///
/// Each concrete endpoint type (TCP, UDP, Serial) will create an instance
/// of `EndpointCore` to handle common tasks like message filtering,
/// deduplication, and routing table updates.
///
/// # Routing-table mutation model
///
/// The hot ingress path only *reads* the routing table (to decide whether an
/// update is needed). New route observations are submitted to a dedicated
/// "Routing Updater" background task through [`EndpointCore::route_update_tx`]
/// — see [`crate::routing::RouteUpdate`] and
/// [`crate::orchestration::spawn_routing_updater`]. This keeps the hot path
/// strictly non-blocking: it never calls `RwLock::write()` itself, which
/// would stall a tokio worker thread under contention.
#[derive(Clone)]
pub struct EndpointCore {
    /// Unique identifier for this endpoint.
    pub id: EndpointId,
    /// Sender half of the global message bus for publishing incoming messages.
    ///
    /// Messages travel as `Arc<RoutedMessage>` so broadcast fan-out is an
    /// atomic refcount per subscriber rather than a full struct clone.
    pub bus_tx: broadcast::Sender<Arc<RoutedMessage>>,
    /// Shared routing table for learning and querying MAVLink network topology.
    ///
    /// The hot path only takes *read* locks here. All writes go through
    /// [`EndpointCore::route_update_tx`] so the async ingress loop never
    /// blocks a tokio worker on a contended write lock.
    pub routing_table: Arc<RoutingTable>,
    /// Channel to the routing updater task. The hot path submits
    /// [`RouteUpdate`]s here with `try_send`; the updater task batches them
    /// and applies them under the write lock on its own.
    pub route_update_tx: mpsc::Sender<RouteUpdate>,
    /// Shared concurrent deduplication instance (sharded for multicore scalability).
    pub dedup: ConcurrentDedup,
    /// Filters applied to messages for this specific endpoint.
    pub filters: EndpointFilters,
    /// Whether this endpoint should update the routing table.
    /// Set to false for TCP server client connections to reduce contention.
    pub update_routing: bool,
    /// Per-endpoint traffic statistics (atomic counters).
    pub stats: Arc<EndpointStats>,
}

/// Get current wall-clock timestamp in microseconds with low overhead.
///
/// Avoids a syscall on every call by capturing the UNIX_EPOCH reference once
/// and adding a monotonic elapsed duration to it.
#[inline(always)]
fn timestamp_us_fast() -> u64 {
    static BASE: std::sync::OnceLock<(Instant, Duration)> = std::sync::OnceLock::new();
    let (start_instant, start_unix) = BASE.get_or_init(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        (Instant::now(), now)
    });

    // Saturate to u64 on extremely long uptimes
    let total = start_unix.saturating_add(start_instant.elapsed());
    // Saturated to u64::MAX before cast — truncation is impossible
    #[allow(clippy::cast_possible_truncation)]
    let result = total.as_micros().min(u64::MAX as u128) as u64;
    result
}

impl EndpointCore {
    /// Handles an incoming MAVLink frame: filter → dedup → observe → publish.
    ///
    /// The orchestrator is intentionally a thin sequence of early-return
    /// guards. Each stage is a private helper on [`EndpointCore`], so the
    /// hot path can be bench-profiled stage-by-stage and individual stages
    /// can be replaced (e.g. swap dedup) without rewriting the others.
    pub fn handle_incoming_frame(&self, frame: MavlinkFrame) {
        // Cache message_id once (avoids repeated calls through Arc later).
        let message_id = frame.message.message_id();

        if !self.accept_frame(&frame.header, message_id) {
            return;
        }
        if self.is_duplicate_frame(&frame) {
            return;
        }
        self.observe_route(&frame.header);
        self.publish_frame(frame, message_id);
    }

    /// Stage 1 — filter.
    ///
    /// Returns `false` when the frame must be dropped before any further
    /// processing: placeholder sys-id 0, or rejection by the configured
    /// per-endpoint filters. Cheap; runs first to short-circuit the rest
    /// of the pipeline.
    #[inline]
    fn accept_frame(&self, header: &MavHeader, message_id: u32) -> bool {
        // Reject invalid/placeholder sources (SysID 0) to avoid polluting
        // the routing table or client connections with phantom systems.
        if header.system_id == 0 {
            trace!(
                "Dropping message with sysid 0 (msg_id {}) from endpoint {}",
                message_id,
                self.id
            );
            return false;
        }
        self.filters.check_incoming(header, message_id)
    }

    /// Stage 2 — deduplication.
    ///
    /// Returns `true` when the dedup window already contains this exact
    /// serialized frame and the orchestrator should drop it. Sharded
    /// internally so concurrent endpoints don't contend on a global lock.
    #[inline]
    fn is_duplicate_frame(&self, frame: &MavlinkFrame) -> bool {
        self.dedup.check_and_insert(&frame.raw_bytes)
    }

    /// Stage 3 — route observation.
    ///
    /// No-op when [`Self::update_routing`] is `false` (e.g. TCP server's
    /// per-client cores reuse the parent's routing knowledge). Otherwise
    /// posts a [`RouteUpdate`] to the dedicated routing-updater task with
    /// `try_send` so the hot path never blocks; if the queue is saturated
    /// the observation is dropped and the next ingress (≤ 1s later by
    /// design) re-checks `needs_update_for_endpoint`.
    fn observe_route(&self, header: &MavHeader) {
        if !self.update_routing {
            return;
        }
        let now = Instant::now();

        // Cheap read first; only enqueue an update when one is actually needed.
        let needs_update = self.routing_table.needs_update_for_endpoint(
            self.id,
            header.system_id,
            header.component_id,
            now,
        );
        if !needs_update {
            return;
        }

        let update = RouteUpdate {
            endpoint_id: self.id,
            sys_id: header.system_id,
            comp_id: header.component_id,
            now,
        };
        if let Err(e) = self.route_update_tx.try_send(update) {
            use mpsc::error::TrySendError;
            match e {
                TrySendError::Full(_) => {
                    trace!(
                        endpoint_id = %self.id,
                        sys_id = header.system_id,
                        comp_id = header.component_id,
                        "Routing update queue full; dropping observation (will retry on next ingress)",
                    );
                }
                TrySendError::Closed(_) => {
                    // Updater task has exited — either during shutdown
                    // or as a bug. Log once so we notice, but keep
                    // processing frames (routing becomes stale, not wrong).
                    trace!(
                        endpoint_id = %self.id,
                        "Routing update channel closed; observation dropped",
                    );
                }
            }
        }
    }

    /// Build a derived core for a per-client TCP server connection.
    ///
    /// Inherits the bus, routing table, route-update channel, dedup, and
    /// filter configuration from this parent core; the only overrides are
    /// the per-client endpoint id (each accepted socket gets its own
    /// `EndpointId`) and a fresh per-client [`EndpointStats`] so traffic
    /// counters stay scoped to the connection. `update_routing` is forced
    /// to `true` because per-client routes are how the server learns
    /// which client to forward replies to.
    pub fn child_for_client(&self, client_id: usize, stats: Arc<EndpointStats>) -> Self {
        Self {
            id: EndpointId(client_id),
            bus_tx: self.bus_tx.clone(),
            routing_table: self.routing_table.clone(),
            route_update_tx: self.route_update_tx.clone(),
            dedup: self.dedup.clone(),
            filters: self.filters.clone(),
            update_routing: true,
            stats,
        }
    }

    /// Stage 4 — publish.
    ///
    /// Wraps the frame in an [`Arc<RoutedMessage>`] (so broadcast fan-out
    /// is an atomic refcount per subscriber, not a clone), pushes it onto
    /// the bus, and bumps the per-endpoint ingress counters.
    fn publish_frame(&self, frame: MavlinkFrame, message_id: u32) {
        // Use raw bytes from parser (zero-copy, no re-serialization needed).
        let raw_len = frame.raw_bytes.len() as u64;
        let serialized_bytes = frame.raw_bytes;

        // Use fast timestamp helper instead of a syscall on every frame.
        let timestamp_us = timestamp_us_fast();

        // Extract target once at ingress; per-endpoint check_outgoing reuses it.
        let target = extract_target(&frame.message);

        // Wrap once so every downstream subscriber shares a single allocation.
        let routed = Arc::new(RoutedMessage {
            source_id: self.id,
            header: frame.header,
            message_id,
            version: frame.version,
            timestamp_us,
            serialized_bytes,
            target,
        });
        if let Err(e) = self.bus_tx.send(routed) {
            warn!("Bus send error: {:?}", e);
            return;
        }

        self.stats.msgs_in.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_in.fetch_add(raw_len, Ordering::Relaxed);
    }

    /// Checks if an outgoing `RoutedMessage` should be sent to this endpoint.
    ///
    /// This method applies two main criteria:
    /// 1. **Source ID Check**: Prevents sending a message back to its original sender endpoint.
    /// 2. **Outgoing Filters**: Applies any endpoint-specific filters defined.
    /// 3. **Routing Table**: Queries the `RoutingTable` to determine if this endpoint is a
    ///    valid destination for the message's target system/component.
    ///
    /// # Arguments
    ///
    /// * `msg` - The `RoutedMessage` to check.
    ///
    /// # Returns
    ///
    /// `true` if the message should be sent to this endpoint, `false` otherwise.
    pub fn check_outgoing(&self, msg: &RoutedMessage) -> bool {
        // Don't send a message back to its source endpoint
        if msg.source_id == self.id {
            return false;
        }

        // Use cached message_id (avoids Arc dereference and vtable call)
        if !self.filters.check_outgoing(&msg.header, msg.message_id) {
            return false;
        }

        // Use cached target from RoutedMessage (computed once at ingress, not per-endpoint)
        let target = msg.target;

        // Optimization: If target is broadcast (system_id == 0), we don't need the routing table lock.
        // We always broadcast unless filtered above or source check fails.
        if target.system_id == 0 {
            return true;
        }

        let rt = self.routing_table.as_ref();
        let should_send = rt.should_send(self.id, target.system_id, target.component_id);

        if !should_send {
            trace!(
                endpoint_id = %self.id,
                source_id = %msg.source_id,
                target_sys = target.system_id,
                target_comp = target.component_id,
                msg_id = msg.message_id,
                "Routing decision: DROP (no route to target)"
            );
        } else {
            trace!(
                endpoint_id = %self.id,
                source_id = %msg.source_id,
                target_sys = target.system_id,
                target_comp = target.component_id,
                msg_id = msg.message_id,
                "Routing decision: FORWARD"
            );
        }

        should_send
    }
}

mod spawn_context;
mod stream_loop;
pub use spawn_context::EndpointSpawnContext;
pub use stream_loop::run_stream_loop;

/// Pull the next bus message, applying the standard `Lagged`/`Closed`
/// bookkeeping shared by every endpoint's send loop (UDP, TCP, Serial).
///
/// On `Lagged(n)` the count is added to `stats.bus_lagged` and an `error!`
/// is emitted (drops are a correctness signal, not noise — every
/// transport's outgoing path must surface them) before transparently
/// re-polling. On `Closed`, returns `None` so the caller can break out.
///
/// # Naming
///
/// Suffix `_blocking` is **not** appropriate — this is `async` — but the
/// returned future does block on `bus_rx.recv().await`. Callers should not
/// hold any locks across this call; the `await` may park the task.
pub async fn recv_next_bus_msg(
    bus_rx: &mut tokio::sync::broadcast::Receiver<Arc<RoutedMessage>>,
    stats: &EndpointStats,
    name: &str,
) -> Option<Arc<RoutedMessage>> {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        match bus_rx.recv().await {
            Ok(msg) => return Some(msg),
            Err(RecvError::Lagged(n)) => {
                stats.bus_lagged.fetch_add(n, Ordering::Relaxed);
                tracing::error!(
                    "{} bus receiver lagged: dropped {} messages (bus_lagged now {})",
                    name,
                    n,
                    stats.bus_lagged.load(Ordering::Relaxed)
                );
                continue;
            }
            Err(RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests;
