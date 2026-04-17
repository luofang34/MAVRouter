//! Endpoint-level parsing tests: flow_control variants, groups, broadcast
//! timeout defaults, sniffer sysids, and baud-rate scalar/array syntax.

#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use crate::config::{BaudRate, Config, EndpointConfig, FlowControl};
use std::str::FromStr;

// ============================================================================
// Flow control
// ============================================================================

#[test]
fn test_flow_control_hardware() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
flow_control = "hardware"
"#;
    let config = Config::from_str(toml).expect("should parse hardware flow_control");
    match &config.endpoint[0] {
        EndpointConfig::Serial { flow_control, .. } => {
            assert_eq!(*flow_control, FlowControl::Hardware);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_flow_control_software() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
flow_control = "software"
"#;
    let config = Config::from_str(toml).expect("should parse software flow_control");
    match &config.endpoint[0] {
        EndpointConfig::Serial { flow_control, .. } => {
            assert_eq!(*flow_control, FlowControl::Software);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_flow_control_none_explicit() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
flow_control = "none"
"#;
    let config = Config::from_str(toml).expect("should parse none flow_control");
    match &config.endpoint[0] {
        EndpointConfig::Serial { flow_control, .. } => {
            assert_eq!(*flow_control, FlowControl::None);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_flow_control_default_when_missing() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
"#;
    let config = Config::from_str(toml).expect("should parse with default flow_control");
    match &config.endpoint[0] {
        EndpointConfig::Serial { flow_control, .. } => {
            assert_eq!(*flow_control, FlowControl::None);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_flow_control_invalid_value() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
flow_control = "rtscts"
"#;
    assert!(
        Config::from_str(toml).is_err(),
        "invalid flow_control value should fail"
    );
}

// ============================================================================
// Groups
// ============================================================================

#[test]
fn test_parse_toml_with_group() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
group = "autopilot"

[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"
baud = 115200
group = "autopilot"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:5761"
mode = "client"
group = "gcs"
"#;
    let config = Config::from_str(toml).expect("should parse config with groups");
    assert_eq!(config.endpoint.len(), 3);
    assert_eq!(config.endpoint[0].group(), Some("autopilot"));
    assert_eq!(config.endpoint[1].group(), Some("autopilot"));
    assert_eq!(config.endpoint[2].group(), Some("gcs"));
}

#[test]
fn test_parse_toml_without_group() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"

[[endpoint]]
type = "tcp"
address = "127.0.0.1:5761"
mode = "client"
"#;
    let config = Config::from_str(toml).expect("should parse config without groups");
    assert_eq!(config.endpoint.len(), 2);
    assert_eq!(config.endpoint[0].group(), None);
    assert_eq!(config.endpoint[1].group(), None);
}

#[test]
fn test_empty_group_string_treated_as_none() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
group = ""
"#;
    let config = Config::from_str(toml).expect("should parse config with empty group");
    assert_eq!(config.endpoint[0].group(), None);
}

// ============================================================================
// Broadcast timeout / sniffer sysids
// ============================================================================

#[test]
fn test_broadcast_timeout_secs_explicit() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "192.168.1.255:14550"
mode = "client"
broadcast_timeout_secs = 10
"#;
    let config = Config::from_str(toml).expect("should parse");
    match &config.endpoint[0] {
        EndpointConfig::Udp {
            broadcast_timeout_secs,
            ..
        } => {
            assert_eq!(*broadcast_timeout_secs, 10);
        }
        _ => panic!("Expected Udp endpoint"),
    }
}

#[test]
fn test_sniffer_sysids_config() {
    let toml = r#"
[general]
sniffer_sysids = [253, 254]

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
"#;
    let config = Config::from_str(toml).expect("should parse config with sniffer_sysids");
    assert_eq!(config.general.sniffer_sysids, vec![253, 254]);
}

#[test]
fn test_sniffer_sysids_default_empty() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
"#;
    let config = Config::from_str(toml).expect("should parse config without sniffer_sysids");
    assert!(config.general.sniffer_sysids.is_empty());
}

#[test]
fn test_broadcast_timeout_secs_default() {
    let toml = r#"
[[endpoint]]
type = "udp"
address = "192.168.1.255:14550"
mode = "client"
"#;
    let config = Config::from_str(toml).expect("should parse");
    match &config.endpoint[0] {
        EndpointConfig::Udp {
            broadcast_timeout_secs,
            ..
        } => {
            assert_eq!(*broadcast_timeout_secs, 5);
        }
        _ => panic!("Expected Udp endpoint"),
    }
}

// ============================================================================
// Baud rate
// ============================================================================

#[test]
fn test_baud_rate_single_value() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = 115200
"#;
    let config = Config::from_str(toml).expect("should parse single baud rate");
    match &config.endpoint[0] {
        EndpointConfig::Serial { baud, .. } => {
            assert!(matches!(baud, BaudRate::Fixed(115200)));
            assert_eq!(baud.rates(), &[115200]);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_baud_rate_array() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = [9600, 57600, 115200]
"#;
    let config = Config::from_str(toml).expect("should parse baud rate array");
    match &config.endpoint[0] {
        EndpointConfig::Serial { baud, .. } => {
            assert!(matches!(baud, BaudRate::Auto(_)));
            assert_eq!(baud.rates(), &[9600, 57600, 115200]);
        }
        _ => panic!("expected Serial endpoint"),
    }
}

#[test]
fn test_baud_rate_empty_array_invalid() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = []
"#;
    assert!(
        Config::from_str(toml).is_err(),
        "empty baud rate array should fail validation"
    );
}

#[test]
fn test_baud_rate_invalid_value_in_array() {
    let toml = r#"
[[endpoint]]
type = "serial"
device = "/dev/ttyUSB0"
baud = [100, 115200]
"#;
    assert!(
        Config::from_str(toml).is_err(),
        "baud rate 100 is too low and should fail validation"
    );
}
