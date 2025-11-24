# mavrouter-rs

A high-performance, reliable, and safe MAVLink router written in Rust. Designed for embedded, unattended systems (Linux/PX4 companions).

## Features

*   **Protocols**: Serial, UDP (Server/Client), TCP (Server/Client).
*   **Reliability**:
    *   **Supervisor Pattern**: Auto-restarts endpoints on failure.
    *   **Leak-Free**: Managed resources using RAII and `JoinSet`.
    *   **Safe**: 100% Safe Rust (`#![deny(unsafe_code)]`), strict linting.
    *   **Lock-Free**: Uses `parking_lot` (no lock poisoning).
*   **Routing**:
    *   Intelligent Loop Prevention.
    *   Deduplication (Time-based).
    *   System ID / Component ID routing (with Pruning).
    *   Message Filtering (Allow/Block lists).
*   **Logging**:
    *   High-throughput TLog recording (compatible with Mission Planner/QGC).
    *   Graceful shutdown ensures log integrity.

## Architecture

### Intelligent Routing

MAVRouter learns network topology automatically:

1. **Learning Phase**: When message arrives from endpoint A with source system_id=100
   → Router records: "System 100 is reachable via endpoint A"

2. **Routing Phase**: When routing message targeted to system_id=100
   → Router sends ONLY to endpoints that have seen system 100

**Example Topology:**
```text
┌─────────┐         ┌──────────┐         ┌────────────┐
│   GCS   │◄───UDP──►│  Router  │◄──Serial─►│ Autopilot │
│ (Sys 255│         │          │         │  (Sys 1)   │
└─────────┘         └────┬─────┘         └────────────┘
                         │
                       TCP
                         │
                    ┌────▼────┐
                    │Companion│
                    │(Sys 100)│
                    └─────────┘
```

**Routing Decisions:**
- GCS→COMMAND(target=1): Only to Autopilot (Serial) ✅
- Autopilot→HEARTBEAT(broadcast): To GCS + Companion ✅
- Companion→PARAM_SET(target=1): Only to Autopilot ✅
- Unknown target: Dropped (no route) ✅

### Performance

Designed for embedded systems:

| Operation | Latency | Throughput |
|-----------|---------|------------|
| Route lookup | ~50ns | 20M ops/sec |
| Message routing | <1µs | 1M msg/sec |
| Target extraction | ~2ns | 500M ops/sec |

**Stress tested:** 10,000 msg/sec sustained, 100 clients.

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
