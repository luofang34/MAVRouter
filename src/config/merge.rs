//! Multi-file configuration merge semantics.
//!
//! For `Option` fields in `GeneralConfig`, the overlay value wins if `Some`.
//! For non-`Option` fields with defaults (`bus_capacity`,
//! `routing_table_ttl_secs`, etc.), the overlay always wins — last-file-wins.
//! Endpoints are concatenated: `self.endpoint` followed by `other.endpoint`.

use super::Config;

impl Config {
    /// Merges two configurations together.
    ///
    /// See the module docs for the exact semantics.
    pub fn merge(mut self, other: Config) -> Config {
        // Option fields: overlay wins if Some
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

        // Non-Option fields: overlay always wins
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
}
