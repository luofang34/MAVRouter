# mavrouter-rs

[![CI](https://github.com/luofang34/mavrouter-rs/workflows/CI/badge.svg)](https://github.com/luofang34/mavrouter-rs/actions)
[![crates.io](https://img.shields.io/crates/v/mavrouter-rs.svg)](https://crates.io/crates/mavrouter-rs)
[![docs.rs](https://docs.rs/mavrouter-rs/badge.svg)](https://docs.rs/mavrouter-rs)
[![License](https://img.shields.io/crates/l/mavrouter-rs.svg)](LICENSE)

A MAVLink router for embedded systems. Focuses on reliability and performance.

## Features

*   **Protocols**: Serial, UDP (Server/Client), TCP (Server/Client).
*   **Reliability**:
    *   **Supervisor Pattern**: Endpoint task restart on failure.
    *   **Resource Management**: Efficient handling of system resources.
    *   **Safety**: Pure Rust implementation, strict linting.
    *   **Concurrency**: Uses `parking_lot` for robust synchronization.
*   **Routing**:
    *   Intelligent routing based on MAVLink target IDs.
    *   Loop prevention mechanisms.
    *   Time-based message deduplication.
    *   Message filtering per endpoint.
*   **Logging**:
    *   TLog recording for MAVLink traffic.
    *   Graceful shutdown behavior.

## Architecture

### Intelligent Routing

MAVRouter learns network topology to route messages efficiently.

**Routing Decisions:**
- Messages targeted to known systems/components are routed to specific endpoints.
- Broadcast messages are routed to all relevant endpoints.
- Messages to unknown targets are dropped.
- Loop prevention is implemented.

## Performance

Designed for embedded environments.

| Operation         | Latency    |
|-------------------|------------|
| Route lookup      | ~50ns      |
| Target extraction | ~2ns       |
| Routing Update    | ~100ns     |
| Pruning (1000 entries) | ~2.8µs  |

## Usage

### Build
```bash
cargo build --release
```

### Configuration
Example `mavrouter.toml`:

```toml
[general]
tcp_port = 5760
log = "logs"
log_telemetry = true
bus_capacity = 1000
routing_table_ttl_secs = 300

[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"
baud = 115200

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
```

### Run
```bash
./target/release/mavrouter-rs --config mavrouter.toml
```

## Development

### Testing

```bash
# Run unit tests
cargo test

# Full validation (before release, runs hardware tests if connected)
./scripts/pre_release_check.sh
```
