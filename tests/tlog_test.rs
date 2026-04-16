//! Integration test for TLog file creation through the public `Router`
//! configuration surface.
//!
//! The low-level file-format contract (timestamp header + raw MAVLink frame)
//! lives as a unit test in `src/endpoints/tlog/tests.rs`; this file only
//! exercises the path a library user would take: write a TOML, hand it to
//! `Router::from_str`, stop, and observe the `*.tlog` file on disk.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use mavrouter::Router;
use serial_test::serial;
use std::time::Duration;

fn create_test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mavrouter_test_{}_{}", name, std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir
}

fn toml_safe_path(path: &std::path::Path) -> String {
    path.to_str()
        .expect("temp dir should be valid UTF-8")
        .replace('\\', "/")
}

fn cleanup_test_dir(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
#[serial]
async fn test_tlog_file_creation() {
    let temp_dir = create_test_dir("tlog_creation");
    let log_path = toml_safe_path(&temp_dir);

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

    // Give the TLOG endpoint time to create its file.
    tokio::time::sleep(Duration::from_millis(500)).await;

    router.stop().await;

    let entries: Vec<_> = std::fs::read_dir(&temp_dir)
        .expect("should read temp dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with("flight_") && s.ends_with(".tlog")
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "Expected at least one flight_*.tlog file in {:?}",
        temp_dir
    );

    cleanup_test_dir(&temp_dir);
}
