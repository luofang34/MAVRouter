use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
use serde::Deserialize;
use std::path::Path;
use std::str::FromStr;
use tokio::fs;

/// Configuration for MAVLink router.
///
/// Loaded from TOML file using [`Config::load`].
///
/// # Example
/// ```toml
/// [general]
/// tcp_port = 5760
/// dedup_period_ms = 100
/// log = "logs"
/// log_telemetry = true
/// bus_capacity = 1000
/// routing_table_ttl_secs = 300
/// routing_table_prune_interval_secs = 60
///
/// [[endpoint]]
/// type = "serial"
/// device = "/dev/ttyACM0"
/// baud = 115200
///
/// [[endpoint]]
/// type = "udp"
/// address = "0.0.0.0:14550"
/// mode = "server"
///
/// [[endpoint]]
/// type = "tcp"
/// address = "127.0.0.1:5761"
/// mode = "client"
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// General router configuration.
    #[serde(default)]
    pub general: GeneralConfig,
    /// List of endpoint configurations.
    #[serde(default)]
    pub endpoint: Vec<EndpointConfig>,
}

/// General router configuration.
#[derive(Debug, Deserialize)]
pub struct GeneralConfig {
    /// Optional TCP server port for general GCS connections.
    pub tcp_port: Option<u16>,
    /// Message deduplication period in milliseconds. Messages with the same
    /// system_id, component_id, message_id, and data will be ignored if
    /// received within this period.
    pub dedup_period_ms: Option<u64>,
    /// Optional path to a directory for logging MAVLink traffic.
    pub log: Option<String>,
    /// Whether to log telemetry data (MAVLink messages) to files.
    #[serde(default)]
    pub log_telemetry: bool,
    /// Capacity of the internal message bus. Typical values are 1000-10000.
    #[serde(default = "default_bus_capacity")]
    pub bus_capacity: usize,
    /// Time-to-live (TTL) for entries in the routing table, in seconds.
    /// Entries older than this will be pruned.
    #[serde(default = "default_routing_table_ttl_secs")]
    pub routing_table_ttl_secs: u64,
    /// Interval at which the routing table is pruned, in seconds.
    #[serde(default = "default_routing_table_prune_interval_secs")]
    pub routing_table_prune_interval_secs: u64,
    /// Stats history retention period in seconds.
    /// Default 86400 seconds (24 hours). Set to 0 to disable stats.
    /// Note: Memory usage ≈ retention_secs * 24 bytes.
    #[serde(default = "default_stats_retention_secs")]
    pub stats_retention_secs: u64,
    /// Stats sampling interval in seconds. Default 1 second.
    #[serde(default = "default_stats_sample_interval_secs")]
    pub stats_sample_interval_secs: u64,
    /// Stats log output interval in seconds. Default 60 seconds.
    #[serde(default = "default_stats_log_interval_secs")]
    pub stats_log_interval_secs: u64,
    /// Path to the Unix socket for querying stats.
    /// Default: None (disabled). Set to a path string to enable (e.g., "/tmp/mavrouter.sock").
    #[serde(default)]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub stats_socket_path: Option<String>,
    /// System IDs that trigger sniffer mode. When an endpoint sees traffic from
    /// a system ID in this list, all messages are forwarded to that endpoint
    /// unconditionally (bypassing routing decisions). Useful for monitoring tools.
    #[serde(default)]
    pub sniffer_sysids: Vec<u8>,
}

fn default_bus_capacity() -> usize {
    5000
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            tcp_port: None,
            dedup_period_ms: None,
            log: None,
            log_telemetry: false,
            bus_capacity: default_bus_capacity(),
            routing_table_ttl_secs: default_routing_table_ttl_secs(),
            routing_table_prune_interval_secs: default_routing_table_prune_interval_secs(),
            stats_retention_secs: default_stats_retention_secs(),
            stats_sample_interval_secs: default_stats_sample_interval_secs(),
            stats_log_interval_secs: default_stats_log_interval_secs(),
            stats_socket_path: None,
            sniffer_sysids: Vec::new(),
        }
    }
}
fn default_routing_table_ttl_secs() -> u64 {
    300
}
fn default_routing_table_prune_interval_secs() -> u64 {
    60
}
fn default_stats_retention_secs() -> u64 {
    86400
}
fn default_stats_sample_interval_secs() -> u64 {
    1
}
fn default_stats_log_interval_secs() -> u64 {
    60
}
fn default_broadcast_timeout_secs() -> u64 {
    5
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
/// Configuration for a specific communication endpoint.
pub enum EndpointConfig {
    /// UDP endpoint configuration.
    Udp {
        /// Address for the UDP endpoint (e.g., "0.0.0.0:14550").
        address: String,
        /// Operating mode for the UDP endpoint (client or server).
        #[serde(default = "default_mode_server")]
        mode: EndpointMode,
        /// Timeout in seconds before reverting from unicast back to broadcast
        /// when using a broadcast target address in client mode.
        /// Only relevant when the target address is a broadcast address.
        #[serde(default = "default_broadcast_timeout_secs")]
        broadcast_timeout_secs: u64,
        /// Filters to apply to messages passing through this endpoint.
        #[serde(flatten)]
        filters: EndpointFilters,
        /// Optional group name for endpoint grouping (redundant links).
        /// Endpoints in the same group share routing knowledge.
        #[serde(default)]
        group: Option<String>,
    },
    /// TCP endpoint configuration.
    Tcp {
        /// Address for the TCP endpoint (e.g., "127.0.0.1:5761").
        address: String,
        /// Operating mode for the TCP endpoint (client or server).
        #[serde(default = "default_mode_client")]
        mode: EndpointMode,
        /// Filters to apply to messages passing through this endpoint.
        #[serde(flatten)]
        filters: EndpointFilters,
        /// Optional group name for endpoint grouping (redundant links).
        /// Endpoints in the same group share routing knowledge.
        #[serde(default)]
        group: Option<String>,
    },
    /// Serial endpoint configuration.
    Serial {
        /// Device path for the serial port (e.g., "/dev/ttyACM0" or "COM1").
        device: String,
        /// Baud rate for the serial connection.
        baud: u32,
        /// Flow control setting for the serial port.
        #[serde(default)]
        flow_control: FlowControl,
        /// Filters to apply to messages passing through this endpoint.
        #[serde(flatten)]
        filters: EndpointFilters,
        /// Optional group name for endpoint grouping (redundant links).
        /// Endpoints in the same group share routing knowledge.
        #[serde(default)]
        group: Option<String>,
    },
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "lowercase")]
/// Operating mode for network endpoints.
pub enum EndpointMode {
    /// Connects as a client to a remote server.
    Client,
    /// Listens for incoming connections as a server.
    Server,
}

/// Flow control setting for serial endpoints.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlowControl {
    /// No flow control (default).
    #[default]
    None,
    /// Hardware (RTS/CTS) flow control.
    Hardware,
    /// Software (XON/XOFF) flow control.
    Software,
}

impl EndpointConfig {
    /// Returns the group name for this endpoint, if set.
    /// Returns `None` if no group is configured or the group string is empty.
    pub fn group(&self) -> Option<&str> {
        let group = match self {
            EndpointConfig::Udp { group, .. } => group.as_deref(),
            EndpointConfig::Tcp { group, .. } => group.as_deref(),
            EndpointConfig::Serial { group, .. } => group.as_deref(),
        };
        // Treat empty string as no group
        group.filter(|g| !g.is_empty())
    }
}

fn default_mode_server() -> EndpointMode {
    EndpointMode::Server
}
fn default_mode_client() -> EndpointMode {
    EndpointMode::Client
}

impl Config {
    /// Merges two configurations together.
    ///
    /// For `Option` fields in `GeneralConfig`, the `other` value wins if `Some`.
    /// For non-`Option` fields with defaults (bus_capacity, routing_table_ttl_secs, etc.),
    /// `other` always wins (last-file-wins semantics).
    /// Endpoints are concatenated: `self.endpoint` followed by `other.endpoint`.
    pub fn merge(mut self, other: Config) -> Config {
        // Option fields: other wins if Some
        if other.general.tcp_port.is_some() {
            self.general.tcp_port = other.general.tcp_port;
        }
        if other.general.dedup_period_ms.is_some() {
            self.general.dedup_period_ms = other.general.dedup_period_ms;
        }
        if other.general.log.is_some() {
            self.general.log = other.general.log;
        }
        if other.general.stats_socket_path.is_some() {
            self.general.stats_socket_path = other.general.stats_socket_path;
        }

        // Non-Option fields: other always wins
        self.general.log_telemetry = other.general.log_telemetry;
        self.general.bus_capacity = other.general.bus_capacity;
        self.general.routing_table_ttl_secs = other.general.routing_table_ttl_secs;
        self.general.routing_table_prune_interval_secs =
            other.general.routing_table_prune_interval_secs;
        self.general.stats_retention_secs = other.general.stats_retention_secs;
        self.general.stats_sample_interval_secs = other.general.stats_sample_interval_secs;
        self.general.stats_log_interval_secs = other.general.stats_log_interval_secs;
        self.general.sniffer_sysids = other.general.sniffer_sysids;

        // Endpoints: concatenate
        self.endpoint.extend(other.endpoint);

        self
    }

    /// Loads all `*.toml` files from a directory, sorted alphabetically,
    /// and merges them sequentially into a single configuration.
    ///
    /// Later files override earlier files for general settings.
    /// Endpoint lists are concatenated across all files.
    /// Non-TOML files in the directory are ignored.
    /// An empty directory produces a valid config with defaults and no endpoints.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the directory cannot be read, or if any
    /// `.toml` file cannot be parsed or the final merged config fails validation.
    pub async fn load_dir(path: impl AsRef<Path>) -> Result<Self> {
        let dir_path = path.as_ref();
        let dir_str = dir_path.display().to_string();

        let mut entries = fs::read_dir(dir_path)
            .await
            .map_err(|e| RouterError::filesystem(&dir_str, e))?;

        let mut toml_files: Vec<String> = Vec::new();

        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let file_path = entry.path();
                    if file_path.extension().and_then(|e| e.to_str()) == Some("toml") {
                        if let Some(name) = file_path.file_name().and_then(|n| n.to_str()) {
                            toml_files.push(name.to_string());
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(RouterError::filesystem(&dir_str, e)),
            }
        }

        toml_files.sort();

        let mut merged = Config::default();

        for filename in &toml_files {
            let file_path = dir_path.join(filename);
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| RouterError::filesystem(file_path.display().to_string(), e))?;

            let config: Config = toml::from_str(&content).map_err(|e| {
                RouterError::config(format!(
                    "Failed to parse config {}: {}",
                    file_path.display(),
                    e
                ))
            })?;

            merged = merged.merge(config);
        }

        merged.validate()?;
        Ok(merged)
    }

    /// Loads configuration from a file and/or directory, merging them.
    ///
    /// If both `file` and `dir` are provided, the file is loaded first,
    /// then the directory configs are merged on top (directory wins for
    /// general settings, endpoints accumulate).
    ///
    /// If neither is provided, returns a default configuration.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if any file cannot be read or parsed,
    /// or if the final merged config fails validation.
    pub async fn load_merged(file: Option<&str>, dir: Option<&str>) -> Result<Self> {
        let mut config = match file {
            Some(path) => Self::load(path).await?,
            None => Config::default(),
        };

        if let Some(dir_path) = dir {
            let dir_config = Self::load_dir(dir_path).await?;
            // dir_config is already validated by load_dir, but we need to
            // re-merge and re-validate the combination
            config = config.merge(dir_config);
            config.validate()?;
        }

        Ok(config)
    }

    /// Parses router configuration from a TOML string.
    ///
    /// This is useful for programmatically generating configurations without
    /// needing to write temporary files.
    ///
    /// # Arguments
    ///
    /// * `toml` - A string containing valid TOML configuration.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the string cannot be parsed or validation fails.
    ///
    /// # Example
    ///
    /// ```
    /// use mavrouter::config::Config;
    /// use std::str::FromStr;
    ///
    /// let toml = r#"
    /// [general]
    /// bus_capacity = 1000
    ///
    /// [[endpoint]]
    /// type = "udp"
    /// address = "0.0.0.0:14550"
    /// mode = "server"
    /// "#;
    ///
    /// let config = Config::from_str(toml).expect("valid config");
    /// assert_eq!(config.endpoint.len(), 1);
    /// ```
    pub fn parse(toml: &str) -> Result<Self> {
        let config: Config = toml::from_str(toml)
            .map_err(|e| RouterError::config(format!("Failed to parse config: {}", e)))?;

        config.validate()?;

        Ok(config)
    }

    /// Loads the router configuration from a TOML file.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the TOML configuration file.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError` if the file cannot be read or parsed,
    /// or if the configuration fails validation.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path_str = path.as_ref().display().to_string();
        let content = fs::read_to_string(path.as_ref())
            .await
            .map_err(|e| RouterError::filesystem(&path_str, e))?;

        Self::parse(&content)
    }

    /// Validates the loaded configuration to check for common errors,
    /// such as duplicate ports, invalid addresses, or invalid baud rates.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError::Config` if the configuration contains invalid or
    /// conflicting settings.
    #[allow(unused_variables)] // 'device' is unused on non-Unix platforms
    pub fn validate(&self) -> Result<()> {
        // Track (protocol, port) pairs -- TCP and UDP on the same port are distinct OS resources
        let mut ports: std::collections::HashSet<(&str, u16)> = std::collections::HashSet::new();

        if let Some(tcp_port) = self.general.tcp_port {
            ports.insert(("tcp", tcp_port));
        }

        for (i, endpoint) in self.endpoint.iter().enumerate() {
            match endpoint {
                EndpointConfig::Tcp { address, .. } => {
                    let addr = address.parse::<std::net::SocketAddr>().map_err(|e| {
                        RouterError::config(format!(
                            "Invalid address in endpoint {}: {} ({})",
                            i, address, e
                        ))
                    })?;

                    if !ports.insert(("tcp", addr.port())) {
                        return Err(RouterError::config(format!(
                            "Duplicate TCP port {} in endpoint {}",
                            addr.port(),
                            i
                        )));
                    }
                }
                EndpointConfig::Udp { address, .. } => {
                    let addr = address.parse::<std::net::SocketAddr>().map_err(|e| {
                        RouterError::config(format!(
                            "Invalid address in endpoint {}: {} ({})",
                            i, address, e
                        ))
                    })?;

                    if !ports.insert(("udp", addr.port())) {
                        return Err(RouterError::config(format!(
                            "Duplicate UDP port {} in endpoint {}",
                            addr.port(),
                            i
                        )));
                    }
                }
                EndpointConfig::Serial { device, baud, .. } => {
                    // Verify baud rate
                    if *baud < 300 || *baud > 4_000_000 {
                        return Err(RouterError::config(format!(
                            "Invalid baud rate in endpoint {}: {} (must be 300-4000000)",
                            i, baud
                        )));
                    }

                    #[cfg(unix)]
                    if !std::path::Path::new(device).exists() {
                        tracing::warn!("Serial device {} does not exist (endpoint {})", device, i);
                    }
                }
            }

            // Validate filters
            let filters = match endpoint {
                EndpointConfig::Udp { ref filters, .. } => filters,
                EndpointConfig::Tcp { ref filters, .. } => filters,
                EndpointConfig::Serial { ref filters, .. } => filters,
            };

            // Helper closure to check msg_ids
            let check_msg_ids = |ids: &ahash::AHashSet<u32>, type_str: &str| -> Result<()> {
                for &msg_id in ids {
                    if msg_id > 65535 {
                        return Err(RouterError::config(format!(
                            "Invalid {} msg_id in endpoint {}: {} (must be <= 65535)",
                            type_str, i, msg_id
                        )));
                    }
                }
                Ok(())
            };

            check_msg_ids(&filters.allow_msg_id_out, "allow_out")?;
            check_msg_ids(&filters.block_msg_id_out, "block_out")?;
            check_msg_ids(&filters.allow_msg_id_in, "allow_in")?;
            check_msg_ids(&filters.block_msg_id_in, "block_in")?;
        }

        if self.general.bus_capacity < 10 {
            return Err(RouterError::config(format!(
                "bus_capacity too small: {}",
                self.general.bus_capacity
            )));
        }

        if self.general.bus_capacity > 1_000_000 {
            return Err(RouterError::config(format!(
                "bus_capacity too large: {} (must be <= 1000000)",
                self.general.bus_capacity
            )));
        }

        if self.general.routing_table_prune_interval_secs == 0 {
            return Err(RouterError::config(
                "routing_table_prune_interval_secs must be greater than 0".to_string(),
            ));
        }

        Ok(())
    }
}

impl FromStr for Config {
    type Err = RouterError;

    /// Parses router configuration from a TOML string.
    ///
    /// This implements the standard `FromStr` trait, allowing the use of
    /// `str.parse::<Config>()` or `Config::from_str(s)`.
    ///
    /// # Example
    ///
    /// ```
    /// use mavrouter::config::Config;
    /// use std::str::FromStr;
    ///
    /// let toml = r#"
    /// [[endpoint]]
    /// type = "udp"
    /// address = "0.0.0.0:14550"
    /// "#;
    ///
    /// let config: Config = toml.parse().expect("valid config");
    /// ```
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Config::parse(s)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

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
        // TCP and UDP on the same port should be allowed (different OS resources)
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
                baud: 100, // Too low
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
                baud: 5_000_000, // Too high
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
                baud: 115200,
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
        // Empty config should be valid (all defaults)
        let config = Config::from_str(toml).expect("empty config should use defaults");
        assert!(config.endpoint.is_empty());
        assert_eq!(config.general.bus_capacity, 5000); // default
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
        // bus_capacity too small should fail validation
        assert!(Config::from_str(toml).is_err());
    }

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
        // bus_capacity is overridden by the second file's default (5000)
        // because non-Option fields always take the last-file-wins value
        assert_eq!(config.general.bus_capacity, default_bus_capacity());
        assert_eq!(config.endpoint.len(), 1);
    }

    #[tokio::test]
    async fn test_load_dir_alphabetical_order() {
        let dir = tempfile::tempdir().expect("create temp dir");

        // First file sets bus_capacity to 1000
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

        // Second file overrides bus_capacity to 2000 and adds endpoint
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
        // Last file wins for non-Option fields
        assert_eq!(config.general.bus_capacity, 2000);
        // Endpoints accumulate from both files
        assert_eq!(config.endpoint.len(), 2);
    }

    #[tokio::test]
    async fn test_load_dir_empty() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = Config::load_dir(dir.path()).await.expect("load empty dir");
        assert!(config.endpoint.is_empty());
        assert_eq!(config.general.bus_capacity, default_bus_capacity());
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
        std::fs::write(dir.path().join("backup.toml.bak"), "not toml extension")
            .expect("write bak");
        std::fs::write(dir.path().join("README.md"), "# Readme").expect("write md");

        let config = Config::load_dir(dir.path()).await.expect("load dir");
        // Only the .toml file should be loaded
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

        // Write main config file
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

        // Create config directory with overrides
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
}
