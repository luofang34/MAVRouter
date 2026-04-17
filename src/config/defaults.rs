//! Default-value functions referenced by `#[serde(default = "…")]` attributes
//! on the config structs.

use super::endpoint::EndpointMode;

pub(super) fn default_bus_capacity() -> usize {
    5000
}

pub(super) fn default_routing_table_ttl_secs() -> u64 {
    300
}

pub(super) fn default_routing_table_prune_interval_secs() -> u64 {
    60
}

pub(super) fn default_stats_retention_secs() -> u64 {
    86400
}

pub(super) fn default_stats_sample_interval_secs() -> u64 {
    1
}

pub(super) fn default_stats_log_interval_secs() -> u64 {
    60
}

pub(super) fn default_broadcast_timeout_secs() -> u64 {
    5
}

pub(super) fn default_mode_server() -> EndpointMode {
    EndpointMode::Server
}

pub(super) fn default_mode_client() -> EndpointMode {
    EndpointMode::Client
}
