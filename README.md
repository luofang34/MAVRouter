# MAVRouter-rs

A MAVLink router written in Rust. Designed for unattended embedded linux systems.

## Features

*   **Protocols**: Serial, UDP (Server/Client), TCP (Server/Client).
*   **Routing**:
    *   Intelligent Loop Prevention.
    *   Deduplication (Time-based).
    *   System ID / Component ID routing (with Pruning).
    *   Message Filtering (Allow/Block lists).
*   **Logging**:
    *   High-throughput TLog recording (compatible with Mission Planner/QGC).
    *   Graceful shutdown ensures log integrity.

## Usage

### Build
```bash
cargo build --release
```

### Configuration
Create `mavrouter.toml`:

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
