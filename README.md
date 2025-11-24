# mavrouter-rs

[![CI](https://github.com/luofang34/mavrouter-rs/workflows/CI/badge.svg)](https://github.com/luofang34/mavrouter-rs/actions)
[![crates.io](https://img.shields.io/crates/v/mavrouter-rs.svg)](https://crates.io/crates/mavrouter-rs)
[![docs.rs](https://docs.rs/mavrouter-rs/badge.svg)](https://docs.rs/mavrouter-rs)
[![License](https://img.shields.io/crates/l/mavrouter-rs.svg)](LICENSE)

A MAVLink router for embedded systems.

## Quick Start

Install from crates.io:
```bash
cargo install mavrouter-rs
```

Basic usage:
```bash
# Create config file
cat > mavrouter.toml <<EOF
[general]
tcp_port = 5760
[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"
baud = 115200
EOF

# Run the router
mavrouter-rs --config mavrouter.toml
```

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

# Statistics Configuration
stats_retention_secs = 86400      # Keep stats for 24 hours
stats_sample_interval_secs = 1    # Sample every 1 second
stats_log_interval_secs = 60      # Log aggregated stats every 60 seconds

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
