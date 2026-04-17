//! Top-level `[general]` section of the router configuration.

use super::defaults::{
    default_bus_capacity, default_routing_table_prune_interval_secs,
    default_routing_table_ttl_secs, default_stats_log_interval_secs, default_stats_retention_secs,
    default_stats_sample_interval_secs,
};
use serde::Deserialize;

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
