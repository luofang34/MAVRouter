use serde::Deserialize;
use std::path::Path;
use anyhow::{Context, Result};
use tokio::fs;
use crate::filter::EndpointFilters;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub endpoint: Vec<EndpointConfig>,
}

#[derive(Debug, Deserialize, Default)]
pub struct GeneralConfig {
    pub tcp_port: Option<u16>,
    pub dedup_period_ms: Option<u64>,
    pub log: Option<String>,
    #[serde(default)]
    pub log_telemetry: bool,
    #[serde(default = "default_bus_capacity")]
    pub bus_capacity: usize,
    #[serde(default = "default_routing_table_ttl_secs")]
    pub routing_table_ttl_secs: u64,
    #[serde(default = "default_routing_table_prune_interval_secs")]
    pub routing_table_prune_interval_secs: u64,
}

fn default_bus_capacity() -> usize { 1000 }
fn default_routing_table_ttl_secs() -> u64 { 300 } 
fn default_routing_table_prune_interval_secs() -> u64 { 60 } 

#[derive(Debug, Deserialize, Clone)] 
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum EndpointConfig {
    Udp {
        address: String,
        #[serde(default = "default_mode_server")]
        mode: EndpointMode,
        #[serde(flatten)]
        filters: EndpointFilters,
    },
    Tcp {
        address: String,
        #[serde(default = "default_mode_client")]
        mode: EndpointMode,
        #[serde(flatten)]
        filters: EndpointFilters,
    },
    Serial {
        device: String,
        baud: u32,
        #[serde(flatten)]
        filters: EndpointFilters,
    },
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum EndpointMode {
    Client,
    Server,
}

fn default_mode_server() -> EndpointMode { EndpointMode::Server }
fn default_mode_client() -> EndpointMode { EndpointMode::Client }

impl Config {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref()).await
            .context("Failed to read config file")?;
        let config: Config = toml::from_str(&content)
            .context("Failed to parse config file")?;
        
        config.validate()?;
        
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ports = std::collections::HashSet::new();

        if let Some(tcp_port) = self.general.tcp_port {
            ports.insert(tcp_port);
        }

        for (i, endpoint) in self.endpoint.iter().enumerate() {
            match endpoint {
                EndpointConfig::Tcp { address, .. } |
                EndpointConfig::Udp { address, .. } => {
                    // Only check port if address parses easily
                    if let Ok(addr) = address.parse::<std::net::SocketAddr>() {
                        if !ports.insert(addr.port()) {
                            anyhow::bail!(
                                "Duplicate port {} in endpoint {}",
                                addr.port(), i
                            );
                        }
                    }
                }
                EndpointConfig::Serial { device, .. } => {
                    #[cfg(unix)]
                    if !std::path::Path::new(device).exists() {
                        tracing::warn!(
                            "Serial device {} does not exist (endpoint {})",
                            device, i
                        );
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
            endpoint: vec![
                EndpointConfig::Udp {
                    address: "127.0.0.1:5760".to_string(),
                    mode: EndpointMode::Server,
                    filters: EndpointFilters::default(),
                }
            ],
        };

        assert!(config.validate().is_err(), "Should detect duplicate port");
    }
}
