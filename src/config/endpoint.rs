//! Endpoint-level configuration: per-`[[endpoint]]` TOML blocks and their
//! supporting enums (`EndpointMode`, `FlowControl`, `BaudRate`).

use super::defaults::{default_broadcast_timeout_secs, default_mode_client, default_mode_server};
use crate::filter::EndpointFilters;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

/// Baud rate configuration for serial endpoints.
/// Accepts either a single baud rate or a list for auto-detection.
#[derive(Debug, Clone)]
pub enum BaudRate {
    /// Fixed baud rate.
    Fixed(u32),
    /// List of baud rates to cycle through for auto-detection.
    Auto(Vec<u32>),
}

impl BaudRate {
    /// Returns the baud rates as a slice.
    /// For `Fixed`, returns a single-element slice.
    /// For `Auto`, returns the full list.
    pub fn rates(&self) -> &[u32] {
        match self {
            BaudRate::Fixed(rate) => std::slice::from_ref(rate),
            BaudRate::Auto(rates) => rates,
        }
    }
}

impl<'de> Deserialize<'de> for BaudRate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BaudRateVisitor;

        impl<'de> Visitor<'de> for BaudRateVisitor {
            type Value = BaudRate;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a baud rate (integer) or array of baud rates")
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<BaudRate, E>
            where
                E: de::Error,
            {
                let value =
                    u32::try_from(value).map_err(|_| E::custom("baud rate too large for u32"))?;
                Ok(BaudRate::Fixed(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<BaudRate, E>
            where
                E: de::Error,
            {
                let value = u32::try_from(value)
                    .map_err(|_| E::custom("baud rate must be a positive integer"))?;
                Ok(BaudRate::Fixed(value))
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<BaudRate, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut rates = Vec::new();
                while let Some(rate) = seq.next_element::<u32>()? {
                    rates.push(rate);
                }
                Ok(BaudRate::Auto(rates))
            }
        }

        deserializer.deserialize_any(BaudRateVisitor)
    }
}

/// Configuration for a specific communication endpoint.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
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
        /// Baud rate for the serial connection. Accepts a single integer or
        /// an array of integers for auto-baud detection.
        baud: BaudRate,
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

/// Operating mode for network endpoints.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "lowercase")]
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
