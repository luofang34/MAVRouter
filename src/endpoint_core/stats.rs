//! Per-endpoint atomic traffic counters and their snapshot view.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

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
