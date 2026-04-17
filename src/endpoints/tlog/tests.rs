//! Unit tests for the TLog (telemetry-log) endpoint.
//!
//! The file-creation test (which exercises only the public `Router` config
//! surface) lives in the `tests/` integration suite. This file covers the
//! low-level `tlog::run(...)` contract: given a bus with a single message,
//! after shutdown the on-disk file must contain exactly a big-endian u64
//! microsecond timestamp followed by the original MAVLink frame bytes.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::arithmetic_side_effects)]

use super::*;
use crate::mavlink_utils::MessageTarget;
use crate::router::{create_bus, EndpointId, RoutedMessage};
use bytes::Bytes;
use mavlink::{MavHeader, MavlinkVersion, Message};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

fn build_heartbeat_bytes() -> (MavHeader, u32, Vec<u8>) {
    let header = MavHeader {
        system_id: 1,
        component_id: 1,
        sequence: 0,
    };
    let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf: Vec<u8> = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();
    (header, msg.message_id(), buf)
}

fn create_test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mavrouter_test_{}_{}", name, std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

fn cleanup_test_dir(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn test_tlog_message_format() {
    let temp_dir = create_test_dir("tlog_format");

    let bus = create_bus(100);
    let bus_rx = bus.subscribe();
    let cancel_token = CancellationToken::new();

    let dir_str = temp_dir
        .to_str()
        .expect("temp dir should be valid UTF-8")
        .to_string();
    let tlog_token = cancel_token.clone();
    let tlog_lagged = Arc::new(AtomicU64::new(0));

    let tlog_handle =
        tokio::spawn(async move { run(dir_str, bus_rx, tlog_lagged, tlog_token).await });

    // Let the TLog endpoint initialise and create the file.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (header, message_id, serialized) = build_heartbeat_bytes();

    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_micros() as u64;

    let routed_msg = RoutedMessage {
        source_id: EndpointId(0),
        header,
        message_id,
        version: MavlinkVersion::V2,
        timestamp_us: now_us,
        serialized_bytes: Bytes::from(serialized.clone()),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    bus.tx
        .send(Arc::new(routed_msg))
        .expect("should broadcast message");

    // Wait past the TLog flush interval (1s) with margin.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    cancel_token.cancel();
    tlog_handle.await.ok();

    let entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .expect("should read temp dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            name_str.starts_with("flight_") && name_str.ends_with(".tlog")
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "Expected at least one flight_*.tlog file in {:?}",
        temp_dir
    );

    let tlog_path = entries[0].path();
    let tlog_data = std::fs::read(&tlog_path).expect("should read tlog file");

    assert!(
        tlog_data.len() >= 8 + serialized.len(),
        "TLOG file too small: {} bytes, expected at least {}",
        tlog_data.len(),
        8 + serialized.len()
    );

    // Record format: u64 big-endian timestamp, then the raw MAVLink frame.
    let ts_bytes: [u8; 8] = tlog_data[0..8].try_into().expect("should get 8 bytes");
    let recorded_timestamp = u64::from_be_bytes(ts_bytes);

    let now_check = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_micros() as u64;

    let five_seconds_us: u64 = 5_000_000;
    assert!(
        recorded_timestamp <= now_check,
        "Recorded timestamp {} is in the future (now: {})",
        recorded_timestamp,
        now_check
    );
    assert!(
        now_check - recorded_timestamp < five_seconds_us,
        "Recorded timestamp {} too old (now: {}, delta: {} us)",
        recorded_timestamp,
        now_check,
        now_check - recorded_timestamp
    );

    let mavlink_bytes = &tlog_data[8..];
    assert_eq!(mavlink_bytes.len(), serialized.len());
    assert_eq!(mavlink_bytes, &serialized[..]);
    assert_eq!(mavlink_bytes[0], 0xFD); // MAVLink v2 magic

    cleanup_test_dir(&temp_dir);
}

// ============================================================================
// Lagged-event counter assertion — mirrors the pattern enforced in
// endpoint_core::tests::lagged for the stream_loop path. TLog subscribes
// to the same bus broadcast channel and must treat `RecvError::Lagged(n)`
// as a counted correctness signal, not noise.
// ============================================================================

/// Flood a capacity-2 bus before the TLog task ever polls, then spawn
/// it with a tiny cancel window. The task's first `recv()` returns one
/// big `Lagged(n)`, which must bump the injected `bus_lagged` counter.
#[tokio::test]
async fn tlog_counts_lagged_from_bus() {
    let temp_dir = create_test_dir("tlog_lagged");

    let bus = create_bus(2);
    let bus_rx = bus.subscribe();
    let cancel_token = CancellationToken::new();
    let lagged = Arc::new(AtomicU64::new(0));

    let (header, message_id, serialized) = build_heartbeat_bytes();
    let template = RoutedMessage {
        source_id: EndpointId(0),
        header,
        message_id,
        version: MavlinkVersion::V2,
        timestamp_us: 0,
        serialized_bytes: Bytes::from(serialized),
        target: MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    // Flood BEFORE spawning — ring buffer silently overwrites old entries.
    const FLOOD_COUNT: usize = 1000;
    for _ in 0..FLOOD_COUNT {
        bus.tx.send(Arc::new(template.clone())).ok();
    }

    let dir_str = temp_dir
        .to_str()
        .expect("temp dir should be valid UTF-8")
        .to_string();
    let tlog_token = cancel_token.clone();
    let tlog_lagged = lagged.clone();
    let tlog_handle =
        tokio::spawn(async move { run(dir_str, bus_rx, tlog_lagged, tlog_token).await });

    // Poll until the counter reflects the lag.
    let deadline = Instant::now() + Duration::from_secs(3);
    while lagged.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }

    cancel_token.cancel();
    tokio::time::timeout(Duration::from_secs(2), tlog_handle)
        .await
        .ok();

    let observed = lagged.load(Ordering::Relaxed);
    assert!(
        observed > 0,
        "expected bus_lagged > 0 after flooding {} messages into a capacity-2 bus, got {}",
        FLOOD_COUNT,
        observed
    );

    cleanup_test_dir(&temp_dir);
}
