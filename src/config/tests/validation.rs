//! Validation tests: duplicate-port detection, bus capacity bounds,
//! prune-interval zero, baud-rate bounds, address parsing, and the
//! `FromStr`/`parse` entry points that surface validation errors.

#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use crate::config::{BaudRate, Config, EndpointConfig, EndpointMode, FlowControl, GeneralConfig};
use crate::filter::EndpointFilters;
use std::str::FromStr;

#[test]
fn test_duplicate_tcp_port_detection() {
    let config = Config {
        general: GeneralConfig {
            tcp_port: Some(5760),
            ..Default::default()
        },
        endpoint: vec![EndpointConfig::Tcp {
            address: "127.0.0.1:5760".to_string(),
            mode: EndpointMode::Client,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };

    assert!(
        config.validate().is_err(),
        "Should detect duplicate TCP port"
    );
}

#[test]
fn test_cross_protocol_same_port_allowed() {
    // TCP and UDP on the same port are distinct OS resources.
    let config = Config {
        general: GeneralConfig {
            tcp_port: Some(5760),
            ..Default::default()
        },
        endpoint: vec![EndpointConfig::Udp {
            address: "127.0.0.1:5760".to_string(),
            mode: EndpointMode::Server,
            broadcast_timeout_secs: 5,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };

    assert!(
        config.validate().is_ok(),
        "TCP and UDP on the same port should be allowed"
    );
}

#[test]
fn test_bus_capacity_too_large() {
    let config = Config {
        general: GeneralConfig {
            bus_capacity: 2_000_000,
            ..Default::default()
        },
        endpoint: vec![],
    };
    assert!(config.validate().is_err());
}

#[test]
fn test_prune_interval_zero() {
    let config = Config {
        general: GeneralConfig {
            routing_table_prune_interval_secs: 0,
            ..Default::default()
        },
        endpoint: vec![],
    };
    assert!(
        config.validate().is_err(),
        "prune_interval_secs = 0 should be rejected"
    );
}

#[test]
fn test_invalid_baud_rate() {
    let config = Config {
        general: GeneralConfig::default(),
        endpoint: vec![EndpointConfig::Serial {
            device: "/dev/ttyUSB0".to_string(),
            baud: BaudRate::Fixed(100), // Too low
            flow_control: FlowControl::None,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };
    assert!(config.validate().is_err());

    let config_high = Config {
        general: GeneralConfig::default(),
        endpoint: vec![EndpointConfig::Serial {
            device: "/dev/ttyUSB0".to_string(),
            baud: BaudRate::Fixed(5_000_000), // Too high
            flow_control: FlowControl::None,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };
    assert!(config_high.validate().is_err());
}

#[test]
fn test_valid_baud_rate() {
    let config = Config {
        general: GeneralConfig::default(),
        endpoint: vec![EndpointConfig::Serial {
            device: "/dev/ttyUSB0".to_string(),
            baud: BaudRate::Fixed(115200),
            flow_control: FlowControl::None,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };
    assert!(config.validate().is_ok());
}

#[test]
fn test_invalid_address() {
    let config = Config {
        general: GeneralConfig::default(),
        endpoint: vec![EndpointConfig::Tcp {
            address: "not_an_address".to_string(),
            mode: EndpointMode::Client,
            filters: EndpointFilters::default(),
            group: None,
        }],
    };
    assert!(config.validate().is_err());
}

#[test]
fn test_bus_capacity_too_small() {
    let config = Config {
        general: GeneralConfig {
            bus_capacity: 5, // Too small
            ..Default::default()
        },
        endpoint: vec![],
    };
    assert!(config.validate().is_err());
}

#[test]
fn test_valid_config() {
    let config = Config {
        general: GeneralConfig {
            tcp_port: Some(5760),
            bus_capacity: 1000,
            ..Default::default()
        },
        endpoint: vec![
            EndpointConfig::Udp {
                address: "0.0.0.0:14550".to_string(),
                mode: EndpointMode::Server,
                broadcast_timeout_secs: 5,
                filters: EndpointFilters::default(),
                group: None,
            },
            EndpointConfig::Tcp {
                address: "127.0.0.1:5761".to_string(),
                mode: EndpointMode::Client,
                filters: EndpointFilters::default(),
                group: None,
            },
        ],
    };
    assert!(config.validate().is_ok());
}

// ============================================================================
// FromStr / parse entry points
// ============================================================================

#[test]
fn test_from_str_valid() {
    let toml = r#"
[general]
bus_capacity = 1000

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
"#;
    let config = Config::from_str(toml).expect("should parse valid TOML");
    assert_eq!(config.endpoint.len(), 1);
    assert_eq!(config.general.bus_capacity, 1000);
}

#[test]
fn test_from_str_invalid_toml() {
    let toml = "this is not valid toml {{{{";
    assert!(Config::from_str(toml).is_err());
}

#[test]
fn test_from_str_empty() {
    let toml = "";
    // Empty config is valid (all defaults).
    let config = Config::from_str(toml).expect("empty config should use defaults");
    assert!(config.endpoint.is_empty());
    assert_eq!(config.general.bus_capacity, 5000);
}

#[test]
fn test_from_str_validation_error() {
    let toml = r#"
[general]
bus_capacity = 5

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
"#;
    // bus_capacity too small should fail validation.
    assert!(Config::from_str(toml).is_err());
}
