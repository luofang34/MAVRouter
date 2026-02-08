#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use bytes::Bytes;
use mavlink::{Message, MavlinkVersion};
use mavrouter::router::{create_bus, EndpointId, RoutedMessage};
use mavrouter::Router;
use serial_test::serial;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Helper: build a serialized MAVLink v2 HEARTBEAT frame and return (header, message_id, bytes).
fn build_heartbeat_bytes() -> (mavlink::MavHeader, u32, Vec<u8>) {
    let header = mavlink::MavHeader {
        system_id: 1,
        component_id: 1,
        sequence: 0,
    };
    let msg =
        mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA::default());
    let mut buf: Vec<u8> = Vec::new();
    mavlink::write_v2_msg(&mut buf, header, &msg).unwrap();
    let message_id = msg.message_id();
    (header, message_id, buf)
}

/// Helper: create a unique temp directory for test isolation.
fn create_test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mavrouter_test_{}_{}", name, std::process::id()));
    // Clean up any leftover from a previous run
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Helper: remove temp directory after test.
fn cleanup_test_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// ---------- Test 1: TLOG File Creation ----------

#[tokio::test]
#[serial]
async fn test_tlog_file_creation() {
    let temp_dir = create_test_dir("tlog_creation");
    let log_path = temp_dir.to_str().expect("temp dir should be valid UTF-8");

    // Configure a router with a UDP endpoint AND TLOG logging enabled
    let toml_cfg = format!(
        r#"
[general]
log = "{log_path}"
log_telemetry = true
bus_capacity = 100

[[endpoint]]
type = "udp"
address = "127.0.0.1:18550"
mode = "server"
"#,
    );

    let router = Router::from_str(&toml_cfg)
        .await
        .expect("router should start");

    // Give the TLOG endpoint time to create the file
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stop the router (triggers flush)
    router.stop().await;

    // Verify that a .tlog file was created in the temp directory
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

    cleanup_test_dir(&temp_dir);
}

// ---------- Test 2: TLOG Message Format ----------

#[tokio::test]
#[serial]
async fn test_tlog_message_format() {
    let temp_dir = create_test_dir("tlog_format");
    let log_path = temp_dir.to_str().expect("temp dir should be valid UTF-8");

    // We use a low-level approach: create a bus, spawn the TLOG endpoint directly,
    // send a RoutedMessage, then verify the file contents.

    let bus = create_bus(100);
    let bus_rx = bus.subscribe();
    let cancel_token = tokio_util::sync::CancellationToken::new();

    let dir_str = log_path.to_string();
    let tlog_token = cancel_token.clone();

    // Spawn the TLOG endpoint
    let tlog_handle = tokio::spawn(async move {
        mavrouter::endpoints::tlog::run(dir_str, bus_rx, tlog_token).await
    });

    // Give TLOG endpoint time to initialize and create the file
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Build a heartbeat message
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
        target: mavrouter::mavlink_utils::MessageTarget {
            system_id: 0,
            component_id: 0,
        },
    };

    // Send the message on the bus
    bus.tx
        .broadcast(routed_msg)
        .await
        .expect("should broadcast message");

    // Wait for flush interval (1 second) + some margin
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Signal shutdown and wait for TLOG task to finish
    cancel_token.cancel();
    let _ = tlog_handle.await;

    // Find the tlog file
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

    // The file should contain at least 8 bytes (timestamp) + the MAVLink frame bytes
    assert!(
        tlog_data.len() >= 8 + serialized.len(),
        "TLOG file too small: {} bytes, expected at least {}",
        tlog_data.len(),
        8 + serialized.len()
    );

    // First 8 bytes: u64 big-endian timestamp
    let ts_bytes: [u8; 8] = tlog_data[0..8].try_into().expect("should get 8 bytes");
    let recorded_timestamp = u64::from_be_bytes(ts_bytes);

    // The timestamp should be reasonable: within 5 seconds of current time
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
        "Recorded timestamp {} is too old (now: {}, delta: {} us)",
        recorded_timestamp,
        now_check,
        now_check - recorded_timestamp
    );

    // After the 8-byte timestamp, the remaining bytes should be valid MAVLink
    let mavlink_bytes = &tlog_data[8..];
    assert_eq!(
        mavlink_bytes.len(),
        serialized.len(),
        "MAVLink bytes length mismatch"
    );
    assert_eq!(
        mavlink_bytes, &serialized[..],
        "MAVLink bytes content mismatch"
    );

    // The first byte of the MAVLink frame should be 0xFD (MAVLink v2 magic)
    assert_eq!(
        mavlink_bytes[0], 0xFD,
        "Expected MAVLink v2 magic byte 0xFD, got 0x{:02X}",
        mavlink_bytes[0]
    );

    cleanup_test_dir(&temp_dir);
}
