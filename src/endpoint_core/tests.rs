//! Unit tests for [`EndpointCore`], [`ExponentialBackoff`], [`EndpointStats`]
//! and [`run_stream_loop`]. Covers:
//! - `ExponentialBackoff` (initial / doubling / cap / reset / custom multiplier)
//! - `EndpointStats` (defaults, increment+snapshot, Display, concurrent access)
//! - `timestamp_us_fast` (monotonic + wall-clock proximity)
//! - `handle_incoming_frame` (happy path, sys-id=0 reject, filter reject, dedup reject)
//! - `check_outgoing` (self-origin reject, filter reject, broadcast pass-through)
//! - `run_stream_loop` loopback (bytes-in → bus, bus → bytes-out)

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use super::*;
use crate::filter::EndpointFilters;
use crate::framing::{MavlinkFrame, StreamParser};
use crate::router::{create_bus, EndpointId, RoutedMessage};
use crate::routing::{RouteUpdate, RoutingTable};
use ahash::AHashSet as HashSet;
use bytes::Bytes;
use mavlink::{common::MavMessage, MavHeader, MavlinkVersion, Message};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// ============================================================================
// ExponentialBackoff
// ============================================================================

#[test]
fn test_exponential_backoff_initial() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    assert_eq!(backoff.next_backoff(), Duration::from_secs(1));
}

#[test]
fn test_exponential_backoff_doubles() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
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
    assert_eq!(backoff.next_backoff(), Duration::from_secs(30)); // cap
    assert_eq!(backoff.next_backoff(), Duration::from_secs(30)); // stays
}

#[test]
fn test_exponential_backoff_reset() {
    let mut backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    backoff.next_backoff();
    backoff.next_backoff();
    backoff.next_backoff();
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

// ============================================================================
// EndpointStats
// ============================================================================

#[test]
fn test_endpoint_stats_default_zero() {
    let stats = EndpointStats::default();
    let snap = stats.snapshot();
    assert_eq!(snap, EndpointStatsSnapshot::default());
    assert_eq!(snap.msgs_in, 0);
    assert_eq!(snap.msgs_out, 0);
    assert_eq!(snap.bytes_in, 0);
    assert_eq!(snap.bytes_out, 0);
    assert_eq!(snap.errors, 0);
}

#[test]
fn test_endpoint_stats_increment_and_snapshot() {
    let stats = EndpointStats::new();
    stats.msgs_in.fetch_add(5, Ordering::Relaxed);
    stats.bytes_in.fetch_add(500, Ordering::Relaxed);
    stats.record_outgoing(100);
    stats.record_outgoing(200);
    stats.errors.fetch_add(1, Ordering::Relaxed);

    let snap = stats.snapshot();
    assert_eq!(snap.msgs_in, 5);
    assert_eq!(snap.bytes_in, 500);
    assert_eq!(snap.msgs_out, 2);
    assert_eq!(snap.bytes_out, 300);
    assert_eq!(snap.errors, 1);
}

#[test]
fn test_endpoint_stats_snapshot_display() {
    let snap = EndpointStatsSnapshot {
        msgs_in: 10,
        msgs_out: 20,
        bytes_in: 1000,
        bytes_out: 2000,
        errors: 3,
        bus_lagged: 7,
    };
    assert_eq!(format!("{}", snap), "in=10/1000 out=20/2000 err=3 lagged=7");
}

#[test]
fn test_endpoint_stats_concurrent_access() {
    use std::thread;

    let stats = Arc::new(EndpointStats::new());
    let num_threads = 8;
    let ops_per_thread = 1000u64;

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let stats = stats.clone();
            thread::spawn(move || {
                for _ in 0..ops_per_thread {
                    stats.msgs_in.fetch_add(1, Ordering::Relaxed);
                    stats.bytes_in.fetch_add(10, Ordering::Relaxed);
                    stats.record_outgoing(20);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let snap = stats.snapshot();
    let total = num_threads as u64 * ops_per_thread;
    assert_eq!(snap.msgs_in, total);
    assert_eq!(snap.bytes_in, total * 10);
    assert_eq!(snap.msgs_out, total);
    assert_eq!(snap.bytes_out, total * 20);
}

// ============================================================================
// timestamp_us_fast
// ============================================================================

#[test]
fn test_timestamp_us_fast_monotonic_walltime() {
    let t1 = timestamp_us_fast();
    let t2 = timestamp_us_fast();
    assert!(t2 >= t1);

    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let tolerance = 5_000_000; // 5 seconds
    assert!(t2 + tolerance >= now);
    assert!(t2 <= now + tolerance);
}

// ============================================================================
// handle_incoming_frame / check_outgoing
// ============================================================================

/// Build a valid MAVLink v2 HEARTBEAT frame and return it as a `MavlinkFrame`,
/// constructed via the real `StreamParser` so `raw_bytes` is populated
/// consistently with the production ingress path.
fn make_heartbeat_frame(system_id: u8, component_id: u8, sequence: u8) -> MavlinkFrame {
    let header = MavHeader {
        system_id,
        component_id,
        sequence,
    };
    let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).expect("write v2");

    let mut parser = StreamParser::new();
    parser.push(&buf);
    parser.parse_next().expect("should parse heartbeat frame")
}

/// Build an `EndpointCore` with an isolated mpsc route-update channel whose
/// receiver is dropped — tests exercising `handle_incoming_frame` don't care
/// whether the route actually lands in a live updater, only that the
/// non-blocking submission path works.
fn make_core(
    id: usize,
    bus_tx: tokio::sync::broadcast::Sender<Arc<RoutedMessage>>,
    routing_table: Arc<parking_lot::RwLock<RoutingTable>>,
    dedup_period: Duration,
    filters: EndpointFilters,
) -> EndpointCore {
    use crate::dedup::ConcurrentDedup;
    let (route_update_tx, _route_update_rx) = tokio::sync::mpsc::channel::<RouteUpdate>(16);
    EndpointCore {
        id: EndpointId(id),
        bus_tx,
        routing_table,
        route_update_tx,
        dedup: ConcurrentDedup::new(dedup_period),
        filters,
        update_routing: true,
        stats: Arc::new(EndpointStats::new()),
    }
}

#[tokio::test]
async fn test_handle_incoming_happy_path() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    let msg = rx.try_recv().expect("Expected a message on the bus");
    assert_eq!(msg.source_id, EndpointId(1));
    assert_eq!(msg.header.system_id, 1);
    assert_eq!(msg.header.component_id, 1);
    assert_eq!(msg.message_id, 0); // HEARTBEAT
}

#[tokio::test]
async fn test_handle_incoming_sysid_zero_rejected() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(0, 1, 0));

    assert!(
        rx.try_recv().is_err(),
        "SysID 0 frame should not appear on bus"
    );
}

#[tokio::test]
async fn test_handle_incoming_filter_rejection() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    // Block msg_id=0 (HEARTBEAT) on incoming.
    let filters = EndpointFilters {
        block_msg_id_in: HashSet::from([0]),
        ..Default::default()
    };

    let core = make_core(1, bus.sender(), routing_table, Duration::ZERO, filters);

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    assert!(
        rx.try_recv().is_err(),
        "Filtered message should not appear on bus"
    );
}

#[tokio::test]
async fn test_handle_incoming_dedup_rejection() {
    let bus = create_bus(100);
    let mut rx = bus.subscribe();
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::from_secs(1),
        EndpointFilters::default(),
    );

    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));
    core.handle_incoming_frame(make_heartbeat_frame(1, 1, 0));

    assert!(rx.try_recv().is_ok(), "First frame should appear");
    assert!(
        rx.try_recv().is_err(),
        "Duplicate frame should be suppressed"
    );
}

#[test]
fn test_check_outgoing_self_origin_rejected() {
    use crate::mavlink_utils::MessageTarget;

    let bus = create_bus(100);
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));
    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    let msg = RoutedMessage {
        source_id: EndpointId(1), // same as core.id
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 0,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    assert!(!core.check_outgoing(&msg));
}

#[test]
fn test_check_outgoing_filter_rejection() {
    use crate::mavlink_utils::MessageTarget;

    let bus = create_bus(100);
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    let filters = EndpointFilters {
        block_msg_id_out: HashSet::from([30]),
        ..Default::default()
    };

    let core = make_core(1, bus.sender(), routing_table, Duration::ZERO, filters);

    let msg = RoutedMessage {
        source_id: EndpointId(2), // different endpoint
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 30, // blocked on outgoing
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    assert!(!core.check_outgoing(&msg));
}

#[test]
fn test_check_outgoing_pass_through() {
    use crate::mavlink_utils::MessageTarget;

    let bus = create_bus(100);
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );

    let msg = RoutedMessage {
        source_id: EndpointId(2),
        header: MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        },
        message_id: 0,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from_static(b"test"),
        target: MessageTarget {
            system_id: 0, // broadcast → short-circuits routing table
            component_id: 0,
        },
    };

    assert!(core.check_outgoing(&msg));
}

// ============================================================================
// run_stream_loop loopback
// ============================================================================

#[tokio::test]
async fn test_stream_loopback() {
    use crate::mavlink_utils::MessageTarget;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    let bus = create_bus(100);
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));
    let token = CancellationToken::new();

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::from_millis(0),
        EndpointFilters::default(),
    );

    let bus_rx = bus.subscribe();

    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    tokio::spawn(async move {
        run_stream_loop(read, write, bus_rx, core, token, "MockSerial".to_string())
            .await
            .unwrap();
    });

    // Direction 1: write into the read side, expect it on the bus.
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        ..Default::default()
    };
    let msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();

    client.write_all(&buf).await.unwrap();

    let mut bus_rx_check = bus.subscribe();
    let received = bus_rx_check.recv().await.unwrap();
    assert_eq!(received.source_id, EndpointId(1));
    assert_eq!(received.header.system_id, 1);

    // Direction 2: inject into the bus, expect it out on the client.
    let header2 = MavHeader {
        system_id: 2,
        component_id: 1,
        ..Default::default()
    };
    let message = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf_out = Vec::new();
    mavlink::write_v2_msg(&mut buf_out, header2, &message).unwrap();

    let msg_out = RoutedMessage {
        source_id: EndpointId(2), // from another endpoint
        header: header2,
        message_id: message.message_id(),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(buf_out),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };
    bus.tx.send(Arc::new(msg_out)).unwrap();

    let mut client_rx_buf = [0u8; 1024];
    let n = client.read(&mut client_rx_buf).await.unwrap();
    assert!(n > 0);
    assert_eq!(client_rx_buf[0], 0xFD); // MAVLink v2 magic
}

// ============================================================================
// Lagged-event counter assertion
// ============================================================================

/// `stream_loop`'s `RecvError::Lagged(n)` handler must bump
/// `EndpointCore::stats::bus_lagged`. Flood a tiny-capacity bus *before*
/// the writer loop is ever spawned — that way the receiver's read position
/// is still at zero when `recv()` is first polled, and the channel returns
/// one big `Lagged(n)` deterministically.
#[tokio::test]
async fn test_bus_lagged_counter_increments_on_flooding() {
    use crate::mavlink_utils::MessageTarget;
    use tokio_util::sync::CancellationToken;

    // Capacity 2 → anything past the second send overwrites older entries.
    let bus = create_bus(2);
    let routing_table = Arc::new(parking_lot::RwLock::new(RoutingTable::new()));
    let cancel = CancellationToken::new();

    let core = make_core(
        1,
        bus.sender(),
        routing_table,
        Duration::ZERO,
        EndpointFilters::default(),
    );
    let stats = Arc::clone(&core.stats);
    let bus_rx = bus.subscribe();

    // Build one representative frame.
    let header = MavHeader {
        system_id: 42,
        component_id: 1,
        sequence: 0,
    };
    let mav_msg = MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut frame = Vec::new();
    mavlink::write_v2_msg(&mut frame, header, &mav_msg).unwrap();
    let template = RoutedMessage {
        source_id: EndpointId(99), // not core.id, so not self-filtered
        header,
        message_id: mav_msg.message_id(),
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(frame),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    // Flood BEFORE spawning the writer task: with no one polling `bus_rx`,
    // the ring buffer silently overwrites older entries. When the writer
    // finally runs, its first `recv()` returns one big `Lagged(n)`.
    const FLOOD_COUNT: usize = 1000;
    for _ in 0..FLOOD_COUNT {
        bus.tx.send(Arc::new(template.clone())).ok();
    }

    // `tokio::io::empty()` would EOF immediately and cause `run_stream_loop`'s
    // outer select to exit before the writer ever polls. Use a duplex pair
    // instead and hold the write side — the reader half then blocks forever
    // on read, letting the writer task drive the bus.
    let (reader_hold, reader) = tokio::io::duplex(1);
    let writer = tokio::io::sink();
    let cancel_for_loop = cancel.clone();
    let loop_handle = tokio::spawn(async move {
        run_stream_loop(
            reader,
            writer,
            bus_rx,
            core,
            cancel_for_loop,
            "LaggedTest".to_string(),
        )
        .await
    });

    // Poll until the counter reflects the lag (the first recv is
    // synchronous after it's scheduled, so this resolves within a few
    // task ticks on any runtime).
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while stats.bus_lagged.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
        tokio::task::yield_now().await;
    }

    cancel.cancel();
    drop(reader_hold);
    tokio::time::timeout(Duration::from_secs(2), loop_handle)
        .await
        .ok();

    let lagged = stats.bus_lagged.load(Ordering::Relaxed);
    assert!(
        lagged > 0,
        "expected bus_lagged > 0 after flooding {} messages into a capacity-2 bus, got {}",
        FLOOD_COUNT,
        lagged
    );
    // Sanity: snapshot() surfaces the same value.
    assert_eq!(stats.snapshot().bus_lagged, lagged);
}
