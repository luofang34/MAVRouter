//! Merge + multi-file loading tests: `Config::merge`, `Config::load_dir`,
//! `Config::load_merged`.

#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use crate::config::{Config, EndpointConfig, EndpointMode, GeneralConfig};
use crate::filter::EndpointFilters;

#[test]
fn test_merge_general_settings() {
    let base = Config {
        general: GeneralConfig {
            tcp_port: Some(5760),
            dedup_period_ms: Some(100),
            bus_capacity: 1000,
            log: Some("logs".to_string()),
            ..Default::default()
        },
        endpoint: vec![EndpointConfig::Udp {
            address: "0.0.0.0:14550".to_string(),
            mode: EndpointMode::Server,
            broadcast_timeout_secs: 5,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };

    let overlay = Config {
        general: GeneralConfig {
            tcp_port: Some(5761),
            bus_capacity: 2000,
            routing_table_ttl_secs: 600,
            ..Default::default()
        },
        endpoint: vec![EndpointConfig::Tcp {
            address: "127.0.0.1:5762".to_string(),
            mode: EndpointMode::Client,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };

    let merged = base.merge(overlay);

    // Option fields: overlay wins if Some
    assert_eq!(merged.general.tcp_port, Some(5761));
    // Option field: base preserved when overlay is None
    assert_eq!(merged.general.dedup_period_ms, Some(100));
    // Non-Option: overlay always wins
    assert_eq!(merged.general.bus_capacity, 2000);
    assert_eq!(merged.general.routing_table_ttl_secs, 600);
    // log: overlay is None, so base preserved
    assert_eq!(merged.general.log, Some("logs".to_string()));
    // Endpoints concatenated
    assert_eq!(merged.endpoint.len(), 2);
}

#[test]
fn test_merge_endpoints_accumulate() {
    let a = Config {
        general: GeneralConfig::default(),
        endpoint: vec![
            EndpointConfig::Udp {
                address: "0.0.0.0:14550".to_string(),
                mode: EndpointMode::Server,
                broadcast_timeout_secs: 5,
                filters: EndpointFilters::default(),
                group: None,
            },
            EndpointConfig::Udp {
                address: "0.0.0.0:14551".to_string(),
                mode: EndpointMode::Server,
                broadcast_timeout_secs: 5,
                filters: EndpointFilters::default(),
                group: None,
            },
        ],
    };

    let b = Config {
        general: GeneralConfig::default(),
        endpoint: vec![EndpointConfig::Tcp {
            address: "127.0.0.1:5760".to_string(),
            mode: EndpointMode::Client,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };

    let merged = a.merge(b);
    assert_eq!(merged.endpoint.len(), 3);
}

#[tokio::test]
async fn test_load_dir_multiple_files() {
    let dir = tempfile::tempdir().expect("create temp dir");

    std::fs::write(
        dir.path().join("00-base.toml"),
        r#"
[general]
tcp_port = 5760
bus_capacity = 1000
"#,
    )
    .expect("write base");

    std::fs::write(
        dir.path().join("10-endpoints.toml"),
        r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#,
    )
    .expect("write endpoints");

    let config = Config::load_dir(dir.path()).await.expect("load dir");
    assert_eq!(config.general.tcp_port, Some(5760));
    // Non-Option fields take the last-file-wins default (5000) when the
    // overlay file doesn't set them explicitly.
    assert_eq!(config.general.bus_capacity, 5000);
    assert_eq!(config.endpoint.len(), 1);
}

#[tokio::test]
async fn test_load_dir_alphabetical_order() {
    let dir = tempfile::tempdir().expect("create temp dir");

    std::fs::write(
        dir.path().join("00-base.toml"),
        r#"
[general]
bus_capacity = 1000

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#,
    )
    .expect("write base");

    std::fs::write(
        dir.path().join("10-override.toml"),
        r#"
[general]
bus_capacity = 2000

[[endpoint]]
type = "tcp"
address = "127.0.0.1:5761"
mode = "client"
"#,
    )
    .expect("write override");

    let config = Config::load_dir(dir.path()).await.expect("load dir");
    assert_eq!(config.general.bus_capacity, 2000);
    assert_eq!(config.endpoint.len(), 2);
}

#[tokio::test]
async fn test_load_dir_empty() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let config = Config::load_dir(dir.path()).await.expect("load empty dir");
    assert!(config.endpoint.is_empty());
    assert_eq!(config.general.bus_capacity, 5000);
}

#[tokio::test]
async fn test_load_dir_ignores_non_toml() {
    let dir = tempfile::tempdir().expect("create temp dir");

    std::fs::write(
        dir.path().join("config.toml"),
        r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#,
    )
    .expect("write toml");

    std::fs::write(dir.path().join("notes.txt"), "not a config file").expect("write txt");
    std::fs::write(dir.path().join("backup.toml.bak"), "not toml extension").expect("write bak");
    std::fs::write(dir.path().join("README.md"), "# Readme").expect("write md");

    let config = Config::load_dir(dir.path()).await.expect("load dir");
    assert_eq!(config.endpoint.len(), 1);
}

#[tokio::test]
async fn test_load_dir_endpoints_from_multiple_files_accumulate() {
    let dir = tempfile::tempdir().expect("create temp dir");

    std::fs::write(
        dir.path().join("01-udp.toml"),
        r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"

[[endpoint]]
type = "udp"
address = "0.0.0.0:14551"
mode = "server"
"#,
    )
    .expect("write udp");

    std::fs::write(
        dir.path().join("02-tcp.toml"),
        r#"
[[endpoint]]
type = "tcp"
address = "127.0.0.1:5760"
mode = "client"
"#,
    )
    .expect("write tcp");

    std::fs::write(
        dir.path().join("03-serial.toml"),
        r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
"#,
    )
    .expect("write serial");

    let config = Config::load_dir(dir.path()).await.expect("load dir");
    assert_eq!(config.endpoint.len(), 4);
}

#[tokio::test]
async fn test_load_merged_file_and_dir() {
    let dir = tempfile::tempdir().expect("create temp dir");

    let file_path = dir.path().join("main.toml");
    std::fs::write(
        &file_path,
        r#"
[general]
tcp_port = 5760
bus_capacity = 1000

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#,
    )
    .expect("write main config");

    let conf_dir = dir.path().join("conf.d");
    std::fs::create_dir(&conf_dir).expect("create conf.d");

    std::fs::write(
        conf_dir.join("10-extra.toml"),
        r#"
[general]
bus_capacity = 3000

[[endpoint]]
type = "tcp"
address = "127.0.0.1:5761"
mode = "client"
"#,
    )
    .expect("write extra");

    let config = Config::load_merged(
        Some(file_path.to_str().expect("path to str")),
        Some(conf_dir.to_str().expect("dir to str")),
    )
    .await
    .expect("load merged");

    // Dir overrides file for non-Option fields
    assert_eq!(config.general.bus_capacity, 3000);
    // File's Option field preserved
    assert_eq!(config.general.tcp_port, Some(5760));
    // Endpoints from both
    assert_eq!(config.endpoint.len(), 2);
}
