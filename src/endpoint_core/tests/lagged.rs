//! `stream_loop`'s Lagged-event counter assertion test.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::arithmetic_side_effects)]

use super::super::*;
use super::helpers::make_core;
use crate::filter::EndpointFilters;
use crate::router::{create_bus, EndpointId, RoutedMessage};
use crate::routing::RoutingTable;
use bytes::Bytes;
use mavlink::{common::MavMessage, MavHeader, MavlinkVersion, Message};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

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
    let routing_table = Arc::new(RoutingTable::new());
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
