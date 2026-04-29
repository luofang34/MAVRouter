//! Unit tests for [`super::EndpointCore`] and friends, partitioned by concern.
//!
//! - `backoff`   — `ExponentialBackoff` (initial / doubling / cap / reset / multiplier)
//! - `helpers`   — shared test fixtures (`make_heartbeat_frame`, `make_core`)
//! - `incoming`  — `handle_incoming_frame` (happy path, sysid=0, filter, dedup)
//! - `lagged`    — `Lagged` accounting on the bus
//! - `loopback`  — `run_stream_loop` end-to-end via `tokio::io::duplex`
//! - `outgoing`  — `check_outgoing` (self-origin, filter, broadcast pass-through)
//! - `stats`     — `EndpointStats` (defaults, increment, Display, concurrent)
//! - `timestamp` — `timestamp_us_fast` monotonic / wall-clock invariants

mod backoff;
mod helpers;
mod incoming;
mod lagged;
mod loopback;
mod outgoing;
mod stats;
mod timestamp;
