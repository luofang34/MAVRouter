# MAVRouter

[![CI](https://github.com/luofang34/mavrouter/workflows/CI/badge.svg)](https://github.com/luofang34/mavrouter/actions)
[![codecov](https://codecov.io/gh/luofang34/mavrouter/branch/main/graph/badge.svg)](https://codecov.io/gh/luofang34/mavrouter)
[![crates.io](https://img.shields.io/crates/v/mavrouter.svg)](https://crates.io/crates/mavrouter)
[![docs.rs](https://docs.rs/mavrouter/badge.svg)](https://docs.rs/mavrouter)
[![License](https://img.shields.io/crates/l/mavrouter.svg)](LICENSE)

A lightweight MAVLink router.

## Quick Start

Install from crates.io:
```bash
cargo install mavrouter
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
mavrouter --config mavrouter.toml
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
./target/release/mavrouter --config mavrouter.toml
```
