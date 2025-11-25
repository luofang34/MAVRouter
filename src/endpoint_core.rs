//! Core logic for MAVLink endpoints.
//!
//! This module defines the shared operational logic for various MAVLink endpoint types
//! (e.g., TCP, UDP, Serial). It includes the `EndpointCore` struct, which encapsulates
//! common resources and filtering mechanisms, and the `run_stream_loop` function,
//! responsible for processing MAVLink messages over asynchronous read/write streams.

use crate::dedup::ConcurrentDedup;
use crate::error::Result;
use crate::filter::EndpointFilters;
use crate::framing::{MavlinkFrame, StreamParser};
use crate::mavlink_utils::extract_target;
use crate::router::{EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use async_broadcast::{Receiver, RecvError, Sender, TryRecvError};
use mavlink::Message;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

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
#[derive(Clone)]
pub struct EndpointCore {
    /// Unique identifier for this endpoint.
    pub id: EndpointId,
    /// Sender half of the global message bus for publishing incoming messages.
    pub bus_tx: Sender<RoutedMessage>,
    /// Shared routing table for learning and querying MAVLink network topology.
    pub routing_table: Arc<RwLock<RoutingTable>>,
    /// Shared concurrent deduplication instance (sharded for multicore scalability).
    pub dedup: ConcurrentDedup,
    /// Filters applied to messages for this specific endpoint.
    pub filters: EndpointFilters,
    /// Whether this endpoint should update the routing table.
    /// Set to false for TCP server client connections to reduce contention.
    pub update_routing: bool,
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
    total.as_micros().min(u64::MAX as u128) as u64
}

impl EndpointCore {
    /// Handles an incoming MAVLink frame, processing it through filters,
    /// deduplication, and routing table updates, then forwarding it to the message bus.
    ///
    /// # Arguments
    ///
    /// * `frame` - The parsed `MavlinkFrame` containing header, message, and version.
    pub async fn handle_incoming_frame(&self, frame: MavlinkFrame) {
        // Cache message_id once (avoids repeated calls through Arc later)
        let message_id = frame.message.message_id();

        // 1. Check filters FIRST (avoid unnecessary work)
        if !self.filters.check_incoming(&frame.header, message_id) {
            return; // Message filtered out, ignore
        }

        // 2. Use raw bytes from parser (zero-copy, no re-serialization needed)
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
                if let Some(mut rt) = self.routing_table.try_write() {
                    rt.update(
                        self.id,
                        frame.header.system_id,
                        frame.header.component_id,
                        now,
                    );
                } else {
                    let mut rt = self.routing_table.write();
                    rt.update(
                        self.id,
                        frame.header.system_id,
                        frame.header.component_id,
                        now,
                    );
                }
            }
        }

        // 5. Send to message bus (use fast timestamp instead of syscall)
        let timestamp_us = timestamp_us_fast();

        // Extract target once, cache in RoutedMessage for all endpoints
        let target = extract_target(&frame.message);

        // Send lightweight RoutedMessage (no Arc allocation - message_id cached)
        if let Err(e) = self
            .bus_tx
            .broadcast(RoutedMessage {
                source_id: self.id,
                header: frame.header,
                message_id,
                version: frame.version,
                timestamp_us,
                serialized_bytes,
                target,
            })
            .await
        {
            warn!("Bus send error: {:?}", e);
        }
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

        if !should_send && target.system_id != 0 {
            // Don't log dropped broadcast as "no route"
            trace!(
                endpoint_id = %self.id,
                target_sys = target.system_id,
                target_comp = target.component_id,
                msg_id = msg.message_id,
                "Routing decision: DROP (no route)"
            );
        }

        should_send
    }
}

/// Runs the main stream processing loop for an endpoint.
///
/// This function sets up two concurrent tasks: one for reading incoming bytes
/// from an `AsyncRead` stream and parsing MAVLink messages, and another for
/// writing outgoing MAVLink messages to an `AsyncWrite` stream.
///
/// # Type Parameters
///
/// * `R` - Type that implements `tokio::io::AsyncRead`, `Unpin`, `Send`, and `'static`.
/// * `W` - Type that implements `tokio::io::AsyncWrite`, `Unpin`, `Send`, and `'static`.
///
/// # Arguments
///
/// * `reader` - An asynchronous reader for incoming bytes.
/// * `writer` - An asynchronous writer for outgoing bytes.
/// * `bus_rx` - Receiver half of the message bus for messages to send out.
/// * `core` - The `EndpointCore` instance providing shared logic and resources.
/// * `cancel_token` - `CancellationToken` to signal graceful shutdown.
/// * `name` - A descriptive name for the endpoint (used in logging).
///
/// # Returns
///
/// A `Result` indicating success or failure. The function runs indefinitely
/// until a read/write error occurs or the `cancel_token` is cancelled.
///
/// # Errors
///
/// Returns an `anyhow::Error` if a critical I/O error occurs on either the read
/// or write stream.
pub async fn run_stream_loop<R, W>(
    mut reader: R,
    writer: W,
    mut bus_rx: Receiver<RoutedMessage>,
    core: EndpointCore,
    cancel_token: CancellationToken,
    name: String,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let core_read = core.clone();
    let name_read = name.clone();

    // Clone CancellationToken for each independent async block/select branch
    let cancel_token_for_reader_loop = cancel_token.clone();
    let cancel_token_for_writer_loop = cancel_token.clone();
    let cancel_token_for_final_select = cancel_token.clone();

    let reader_loop = async move {
        let mut parser = StreamParser::new();
        let mut buf = [0u8; 4096];

        loop {
            tokio::select! {
                _ = cancel_token_for_reader_loop.cancelled() => break,
                read_res = reader.read(&mut buf) => {
                    match read_res {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            parser.push(&buf[..n]);
                            while let Some(frame) = parser.parse_next() {
                                core_read.handle_incoming_frame(frame).await;
                            }
                        }
                        Err(e) => {
                            error!("{} read error: {}", name_read, e);
                            break;
                        }
                    }
                }
            }
        }
    };

    let writer_loop = async move {
        let mut writer = BufWriter::new(writer);

        loop {
            tokio::select! {
                _ = cancel_token_for_writer_loop.cancelled() => break,
                msg_res = bus_rx.recv() => {
                    match msg_res {
                        Ok(msg) => {
                            if !core.check_outgoing(&msg) {
                                continue;
                            }

                            if let Err(e) = writer.write_all(&msg.serialized_bytes).await {
                                debug!("{} write error: {}", name, e);
                                break;
                            }

                            // Optimistic batching - process a fixed batch to reduce wakeups
                            const BATCH_SIZE: usize = 512;
                            for _ in 0..BATCH_SIZE {
                                match bus_rx.try_recv() {
                                    Ok(m) => {
                                        if core.check_outgoing(&m) {
                                            if let Err(e) = writer.write_all(&m.serialized_bytes).await {
                                                debug!("{} write error: {}", name, e);
                                                return;
                                            }
                                        }
                                    }
                                    Err(TryRecvError::Empty) => break,
                                    Err(TryRecvError::Overflowed(n)) => {
                                        warn!("{} Sender lagged: missed {} messages", name, n);
                                    }
                                    Err(TryRecvError::Closed) => return,
                                }
                            }

                            // Only flush when the queue is drained to avoid extra syscalls under load
                            if let Err(e) = writer.flush().await {
                                debug!("{} flush error: {}", name, e);
                                break;
                            }
                        }
                        Err(RecvError::Overflowed(n)) => {
                            warn!("{} Sender lagged: missed {} messages", name, n);
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    tokio::select! {
        _ = reader_loop => Ok(()),
        _ = writer_loop => Ok(()),
        _ = cancel_token_for_final_select.cancelled() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn test_exponential_backoff_initial() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
    }

    #[test]
    fn test_exponential_backoff_doubles() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(2));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(4));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(8));
    }

    #[test]
    fn test_exponential_backoff_caps_at_max() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_secs(10), Duration::from_secs(30), 2.0);
        assert_eq!(backoff.next_backoff(), Duration::from_secs(10));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(20));
        // Would be 40, but capped at 30
        assert_eq!(backoff.next_backoff(), Duration::from_secs(30));
        // Stays at max
        assert_eq!(backoff.next_backoff(), Duration::from_secs(30));
    }

    #[test]
    fn test_exponential_backoff_reset() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
        backoff.next_backoff(); // 1
        backoff.next_backoff(); // 2
        backoff.next_backoff(); // 4

        backoff.reset();
        assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
    }

    #[test]
    fn test_exponential_backoff_custom_multiplier() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(100), 3.0);
        assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(3));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(9));
        assert_eq!(backoff.next_backoff(), Duration::from_secs(27));
    }

    #[test]
    fn test_timestamp_us_fast_monotonic_walltime() {
        let t1 = timestamp_us_fast();
        let t2 = timestamp_us_fast();
        assert!(t2 >= t1);

        // Should be near wall-clock now (within a generous window to avoid flakiness)
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        let tolerance = 5_000_000; // 5 seconds
        assert!(t2 + tolerance >= now);
        assert!(t2 <= now + tolerance);
    }
}
