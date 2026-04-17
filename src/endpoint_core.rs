//! Core logic for MAVLink endpoints.
//!
//! This module defines the shared operational logic for various MAVLink endpoint types
//! (e.g., TCP, UDP, Serial). It includes the `EndpointCore` struct, which encapsulates
//! common resources and filtering mechanisms, and the `run_stream_loop` function,
//! responsible for processing MAVLink messages over asynchronous read/write streams.

use crate::dedup::ConcurrentDedup;
use crate::filter::EndpointFilters;
use crate::framing::MavlinkFrame;
use crate::mavlink_utils::extract_target;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::{RouteUpdate, RoutingTable};
use mavlink::Message;
use parking_lot::RwLock;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast::{self};
use tokio::sync::mpsc;
use tracing::{trace, warn};

/// Per-endpoint traffic statistics tracked via atomic counters.
///
/// These counters are incremented using `Ordering::Relaxed` for minimal overhead.
/// Use [`EndpointStats::snapshot`] to read a consistent point-in-time view.
#[derive(Debug, Default)]
pub struct EndpointStats {
    /// Number of messages received (ingress) on this endpoint.
    pub msgs_in: AtomicU64,
    /// Number of messages sent (egress) from this endpoint.
    pub msgs_out: AtomicU64,
    /// Total bytes received (ingress) on this endpoint.
    pub bytes_in: AtomicU64,
    /// Total bytes sent (egress) from this endpoint.
    pub bytes_out: AtomicU64,
    /// Number of errors encountered on this endpoint.
    pub errors: AtomicU64,
    /// Number of messages dropped because this endpoint's broadcast receiver
    /// lagged behind the bus and the channel overwrote un-read entries.
    /// Incremented by the count reported in the `Lagged(n)` branch, not by 1.
    pub bus_lagged: AtomicU64,
}

impl EndpointStats {
    /// Creates a new `EndpointStats` with all counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a point-in-time snapshot of the current counter values.
    pub fn snapshot(&self) -> EndpointStatsSnapshot {
        EndpointStatsSnapshot {
            msgs_in: self.msgs_in.load(Ordering::Relaxed),
            msgs_out: self.msgs_out.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            bus_lagged: self.bus_lagged.load(Ordering::Relaxed),
        }
    }

    /// Records an outgoing message with the given byte count.
    pub fn record_outgoing(&self, bytes: u64) {
        self.msgs_out.fetch_add(1, Ordering::Relaxed);
        self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// A point-in-time snapshot of [`EndpointStats`] with plain `u64` fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointStatsSnapshot {
    /// Number of messages received (ingress).
    pub msgs_in: u64,
    /// Number of messages sent (egress).
    pub msgs_out: u64,
    /// Total bytes received (ingress).
    pub bytes_in: u64,
    /// Total bytes sent (egress).
    pub bytes_out: u64,
    /// Number of errors encountered.
    pub errors: u64,
    /// Number of bus messages the receiver missed because it fell behind.
    pub bus_lagged: u64,
}

impl fmt::Display for EndpointStatsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "in={}/{} out={}/{} err={} lagged={}",
            self.msgs_in,
            self.bytes_in,
            self.msgs_out,
            self.bytes_out,
            self.errors,
            self.bus_lagged,
        )
    }
}

/// Exponential backoff helper for connection retries.
#[derive(Debug)]
pub struct ExponentialBackoff {
    current: Duration,
    min: Duration,
    max: Duration,
    multiplier: f64,
}

impl ExponentialBackoff {
    /// Creates a new `ExponentialBackoff` instance.
    ///
    /// # Arguments
    ///
    /// * `min` - The minimum (initial) delay duration.
    /// * `max` - The maximum delay duration.
    /// * `multiplier` - The factor by which the delay increases after each call to `next()`.
    pub fn new(min: Duration, max: Duration, multiplier: f64) -> Self {
        Self {
            current: min,
            min,
            max,
            multiplier,
        }
    }

    /// Returns the current delay duration and updates it for the next call.
    ///
    /// The returned duration is the one that should be waited *now*. The internal
    /// state is updated to `current * multiplier` (capped at `max`) for the *next* call.
    pub fn next_backoff(&mut self) -> Duration {
        let wait = self.current;
        self.current = std::cmp::min(
            self.max,
            Duration::from_secs_f64(self.current.as_secs_f64() * self.multiplier),
        );
        wait
    }

    /// Resets the backoff delay to the minimum value.
    ///
    /// This should be called when a connection is successfully established or
    /// a task runs successfully for a sufficient duration.
    pub fn reset(&mut self) {
        self.current = self.min;
    }
}

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
    pub routing_table: Arc<RwLock<RoutingTable>>,
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
    /// Handles an incoming MAVLink frame, processing it through filters,
    /// deduplication, and routing table updates, then forwarding it to the message bus.
    ///
    /// # Arguments
    ///
    /// * `frame` - The parsed `MavlinkFrame` containing header, message, and version.
    pub fn handle_incoming_frame(&self, frame: MavlinkFrame) {
        // Cache message_id once (avoids repeated calls through Arc later)
        let message_id = frame.message.message_id();

        // Reject invalid/placeholder sources (SysID 0) to avoid polluting routing/clients
        if frame.header.system_id == 0 {
            trace!(
                "Dropping message with sysid 0 (msg_id {}) from endpoint {}",
                message_id,
                self.id
            );
            return;
        }

        // 1. Check filters FIRST (avoid unnecessary work)
        if !self.filters.check_incoming(&frame.header, message_id) {
            return; // Message filtered out, ignore
        }

        // 2. Use raw bytes from parser (zero-copy, no re-serialization needed)
        // MAVLink frames are max 280 bytes, always fits u64
        #[allow(clippy::cast_possible_truncation)]
        let raw_len = frame.raw_bytes.len() as u64;
        let serialized_bytes = frame.raw_bytes;

        // 3. Check deduplication (sharded for concurrent access - no global lock)
        if self.dedup.check_and_insert(&serialized_bytes) {
            return; // Duplicate message, ignore
        }

        // 4. Update routing table (skip for endpoints that don't need routing, e.g., TCP server clients)
        if self.update_routing {
            let now = Instant::now();

            // Cheap read first; only take write lock when an update is actually needed
            let needs_update = {
                let rt = self.routing_table.read();
                rt.needs_update_for_endpoint(
                    self.id,
                    frame.header.system_id,
                    frame.header.component_id,
                    now,
                )
            };

            if needs_update {
                // Submit to the dedicated routing updater task. `try_send` never
                // blocks — if the queue is saturated we drop this observation and
                // will retry the next time `needs_update_for_endpoint` fires
                // (≤ 1s later by design). Dropping is strictly preferable to
                // taking a blocking write lock on a tokio worker thread.
                let update = RouteUpdate {
                    endpoint_id: self.id,
                    sys_id: frame.header.system_id,
                    comp_id: frame.header.component_id,
                    now,
                };
                if let Err(e) = self.route_update_tx.try_send(update) {
                    use mpsc::error::TrySendError;
                    match e {
                        TrySendError::Full(_) => {
                            trace!(
                                endpoint_id = %self.id,
                                sys_id = frame.header.system_id,
                                comp_id = frame.header.component_id,
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
        }

        // 5. Send to message bus (use fast timestamp instead of syscall)
        let timestamp_us = timestamp_us_fast();

        // Extract target once, cache in RoutedMessage for all endpoints
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

        // 6. Record incoming message stats
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

        let rt = self.routing_table.read();
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

mod stream_loop;
pub use stream_loop::run_stream_loop;

#[cfg(test)]
mod tests;
