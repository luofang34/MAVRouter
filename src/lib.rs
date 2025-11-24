#![deny(unsafe_code)]
#![deny(clippy::unwrap_used)]

pub mod config;
pub mod router;
pub mod endpoint_core;
pub mod endpoints {
    pub mod udp;
    pub mod tcp;
    pub mod serial;
    pub mod tlog;
}
pub mod dedup;
pub mod routing;
pub mod filter;
pub mod mavlink_utils;
pub mod lock_helpers;
pub mod framing;
