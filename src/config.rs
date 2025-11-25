use crate::error::{Result, RouterError};
use crate::filter::EndpointFilters;
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
    pub stats_socket_path: Option<String>,
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
    /// Returns a `RouterError` if the file cannot be read or parsed,
    /// or if the configuration fails validation.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path_str = path.as_ref().display().to_string();
        let content = fs::read_to_string(path.as_ref())
            .await
            .map_err(|e| RouterError::filesystem(&path_str, e))?;

        let config: Config = toml::from_str(&content)
            .map_err(|e| RouterError::config(format!("Failed to parse config file: {}", e)))?;

        config.validate()?;

        Ok(config)
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
        let mut ports = std::collections::HashSet::new();

        if let Some(tcp_port) = self.general.tcp_port {
            ports.insert(tcp_port);
        }

        for (i, endpoint) in self.endpoint.iter().enumerate() {
            match endpoint {
                EndpointConfig::Tcp { address, .. } | EndpointConfig::Udp { address, .. } => {
                    let addr = address.parse::<std::net::SocketAddr>().map_err(|e| {
                        RouterError::config(format!(
                            "Invalid address in endpoint {}: {} ({})",
                            i, address, e
                        ))
                    })?;

                    if !ports.insert(addr.port()) {
                        return Err(RouterError::config(format!(
                            "Duplicate port {} in endpoint {}",
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
                EndpointConfig::Udp { filters, .. } => filters,
                EndpointConfig::Tcp { filters, .. } => filters,
                EndpointConfig::Serial { filters, .. } => filters,
            };

            // Helper closure to check msg_ids
            let check_msg_ids =
                |ids: &std::collections::HashSet<u32>, type_str: &str| -> Result<()> {
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

    #[test]
    fn test_invalid_baud_rate() {
        let config = Config {
            general: GeneralConfig::default(),
            endpoint: vec![EndpointConfig::Serial {
                device: "/dev/ttyUSB0".to_string(),
                baud: 100, // Too low
                filters: EndpointFilters::default(),
            }],
        };
        assert!(config.validate().is_err());

        let config_high = Config {
            general: GeneralConfig::default(),
            endpoint: vec![EndpointConfig::Serial {
                device: "/dev/ttyUSB0".to_string(),
                baud: 5_000_000, // Too high
                filters: EndpointFilters::default(),
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
                filters: EndpointFilters::default(),
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
                    filters: EndpointFilters::default(),
                },
                EndpointConfig::Tcp {
                    address: "127.0.0.1:5761".to_string(),
                    mode: EndpointMode::Client,
                    filters: EndpointFilters::default(),
                },
            ],
        };
        assert!(config.validate().is_ok());
    }
}
