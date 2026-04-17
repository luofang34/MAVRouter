//! Config-wide validation: duplicate ports, invalid addresses, out-of-range
//! baud rates, oversize `msg_id` filter entries, and a few `[general]`
//! sanity bounds.

use super::endpoint::EndpointConfig;
use super::Config;
use crate::error::{Result, RouterError};

impl Config {
    /// Validates the loaded configuration to check for common errors,
    /// such as duplicate ports, invalid addresses, or invalid baud rates.
    ///
    /// # Errors
    ///
    /// Returns a `RouterError::Config` if the configuration contains invalid or
    /// conflicting settings.
    #[allow(unused_variables)] // 'device' is unused on non-Unix platforms
    pub fn validate(&self) -> Result<()> {
        // Track (protocol, port) pairs -- TCP and UDP on the same port are
        // distinct OS resources.
        let mut ports: std::collections::HashSet<(&str, u16)> = std::collections::HashSet::new();

        if let Some(tcp_port) = self.general.tcp_port {
            ports.insert(("tcp", tcp_port));
        }

        for (i, endpoint) in self.endpoint.iter().enumerate() {
            validate_endpoint(i, endpoint, &mut ports)?;

            // Validate filter msg_id ranges
            let filters = match endpoint {
                EndpointConfig::Udp { ref filters, .. } => filters,
                EndpointConfig::Tcp { ref filters, .. } => filters,
                EndpointConfig::Serial { ref filters, .. } => filters,
            };

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

/// Validate address / baud rate of a single endpoint and record its port in
/// the `ports` set (rejecting duplicates within the same protocol).
#[allow(unused_variables)] // `device` unused on non-unix
fn validate_endpoint<'a>(
    i: usize,
    endpoint: &'a EndpointConfig,
    ports: &mut std::collections::HashSet<(&'a str, u16)>,
) -> Result<()> {
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
            let rates = baud.rates();
            if rates.is_empty() {
                return Err(RouterError::config(format!(
                    "Empty baud rate list in endpoint {}",
                    i
                )));
            }
            for rate in rates {
                if *rate < 300 || *rate > 4_000_000 {
                    return Err(RouterError::config(format!(
                        "Invalid baud rate in endpoint {}: {} (must be 300-4000000)",
                        i, rate
                    )));
                }
            }

            #[cfg(unix)]
            if !std::path::Path::new(device).exists() {
                tracing::warn!("Serial device {} does not exist (endpoint {})", device, i);
            }
        }
    }
    Ok(())
}
