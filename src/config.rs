use crate::filter::EndpointFilters;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
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
#[derive(Debug, Deserialize)]
pub struct Config {
    /// General router configuration.
    #[serde(default)]
    pub general: GeneralConfig,
    /// List of endpoint configurations.
    #[serde(default)]
    pub endpoint: Vec<EndpointConfig>,
}

/// General router configuration.
#[derive(Debug, Deserialize, Default)]
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
    pub stats_socket_path: Option<String>,
}

fn default_bus_capacity() -> usize {
    1000
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
        /// Filters to apply to messages passing through this endpoint.
        #[serde(flatten)]
        filters: EndpointFilters,
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
    },
    /// Serial endpoint configuration.
    Serial {
        /// Device path for the serial port (e.g., "/dev/ttyACM0" or "COM1").
        device: String,
        /// Baud rate for the serial connection.
        baud: u32,
        /// Filters to apply to messages passing through this endpoint.
        #[serde(flatten)]
        filters: EndpointFilters,
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

fn default_mode_server() -> EndpointMode {
    EndpointMode::Server
}
fn default_mode_client() -> EndpointMode {
    EndpointMode::Client
}

impl Config {
    /// Loads the router configuration from a TOML file.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the TOML configuration file.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` if the file cannot be read or parsed,
    /// or if the configuration fails validation.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .await
            .context("Failed to read config file")?;
        let config: Config = toml::from_str(&content).context("Failed to parse config file")?;

        config.validate()?;

        Ok(config)
    }

    /// Validates the loaded configuration to check for common errors,
    /// such as duplicate ports or non-existent serial devices (on Unix).
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` if the configuration contains invalid or
    /// conflicting settings.
    #[allow(unused_variables)] // 'device' is unused on non-Unix platforms
    pub fn validate(&self) -> Result<()> {
        let mut ports = std::collections::HashSet::new();

        if let Some(tcp_port) = self.general.tcp_port {
            ports.insert(tcp_port);
        }

        for (i, endpoint) in self.endpoint.iter().enumerate() {
            match endpoint {
                EndpointConfig::Tcp { address, .. } | EndpointConfig::Udp { address, .. } => {
                    // Only check port if address parses easily
                    if let Ok(addr) = address.parse::<std::net::SocketAddr>() {
                        if !ports.insert(addr.port()) {
                            anyhow::bail!("Duplicate port {} in endpoint {}", addr.port(), i);
                        }
                    }
                }
                EndpointConfig::Serial { device, .. } =>
                {
                    #[cfg(unix)]
                    if !std::path::Path::new(device).exists() {
                        tracing::warn!("Serial device {} does not exist (endpoint {})", device, i);
                    }
                }
            }
        }

        if self.general.bus_capacity < 10 {
            anyhow::bail!("bus_capacity too small: {}", self.general.bus_capacity);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duplicate_port_detection() {
        let config = Config {
            general: GeneralConfig {
                tcp_port: Some(5760),
                ..Default::default()
            },
            endpoint: vec![EndpointConfig::Udp {
                address: "127.0.0.1:5760".to_string(),
                mode: EndpointMode::Server,
                filters: EndpointFilters::default(),
            }],
        };

        assert!(config.validate().is_err(), "Should detect duplicate port");
    }
}
