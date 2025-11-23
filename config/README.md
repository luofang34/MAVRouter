# Configuration Examples

This directory contains example TOML configuration files for the MAVLink Router.

## Available Configurations

### 1. `mavrouter.toml` - Basic Configuration
**Use case**: Simple TCP + UDP router for ground control station (GCS) communication

```toml
[general]
tcp_port = 5760  # Implicit TCP server

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
```

**Features**:
- TCP server on port 5760 (automatic, implicit)
- UDP server on port 14550 for GCS connections
- No logging, no filtering
- Suitable for basic MAVLink message routing

**Usage**:
```bash
cargo run -- --config config/mavrouter.toml
```

---

### 2. `mavrouter_test.toml` - Hardware Testing Configuration
**Use case**: Testing with real autopilot hardware via serial port

```toml
[general]
tcp_port = 5760
log = "logs"
log_telemetry = true

[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"
baud = 115200

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
```

**Features**:
- Serial connection to autopilot (PX4, ArduPilot, etc.) at 115200 baud
- TCP server on port 5760 for GCS/tools
- UDP server on port 14550 for GCS
- **Telemetry logging enabled** - saves all MAVLink traffic to `logs/` directory in TLog format

**Usage**:
```bash
# Ensure your autopilot is connected to /dev/ttyACM0
# Adjust device path and baud rate if needed
cargo run -- --config config/mavrouter_test.toml
```

**Note**: You may need to modify `/dev/ttyACM0` to match your actual serial device (check with `ls /dev/tty*`).

---

### 3. `mavrouter_filter_allow.toml` - Allow List Example
**Use case**: Demonstrating message filtering with allow list (whitelist)

```toml
[[endpoint]]
type = "tcp"
address = "0.0.0.0:5760"
mode = "server"
allow_msg_id_out = [0]  # Only allow HEARTBEAT messages
```

**Features**:
- Serial connection to autopilot
- TCP server that **only forwards HEARTBEAT messages** (ID 0)
- All other message types are blocked on outbound traffic
- Useful for bandwidth-limited connections or testing specific message flows

**Usage**:
```bash
cargo run -- --config config/mavrouter_filter_allow.toml
```

**Testing**:
Connect to TCP port 5760 and verify you only receive HEARTBEAT messages.

---

### 4. `mavrouter_filter_block.toml` - Block List Example
**Use case**: Demonstrating message filtering with block list (blacklist)

```toml
[[endpoint]]
type = "tcp"
address = "0.0.0.0:5760"
mode = "server"
block_msg_id_out = [0]  # Block HEARTBEAT messages
```

**Features**:
- Serial connection to autopilot
- TCP server that **blocks HEARTBEAT messages** (ID 0) from being forwarded
- All other messages pass through normally
- Useful for reducing noise or testing system behavior without heartbeats

**Usage**:
```bash
cargo run -- --config config/mavrouter_filter_block.toml
```

**Testing**:
Connect to TCP port 5760 and verify you receive all messages except HEARTBEAT.

---

## Configuration Reference

### General Section

```toml
[general]
bus_capacity = 1000                        # Message bus queue size (default: 1000)
dedup_period_ms = 100                      # Deduplication window in ms (0 = disabled)
routing_table_prune_interval_secs = 30    # How often to clean routing table
routing_table_ttl_secs = 60               # Route entry expiration time
tcp_port = 5760                           # Optional implicit TCP server port
log = "logs"                              # Log directory (optional)
log_telemetry = true                      # Enable TLog telemetry logging
```

### Endpoint Types

#### TCP Endpoint
```toml
[[endpoint]]
type = "tcp"
address = "127.0.0.1:5760"
mode = "client"  # or "server"

# Optional filters
allow_msg_id_in = [0, 1, 30]   # Whitelist for incoming messages
allow_msg_id_out = [0, 1, 30]  # Whitelist for outgoing messages
block_msg_id_in = [999]        # Blacklist for incoming messages
block_msg_id_out = [999]       # Blacklist for outgoing messages
```

**Modes**:
- `client`: Connect to remote TCP server
- `server`: Accept incoming TCP connections

#### UDP Endpoint
```toml
[[endpoint]]
type = "udp"
address = "127.0.0.1:14550"
mode = "client"  # or "server"

# Optional filters (same as TCP)
```

**Modes**:
- `client`: Send to remote UDP address
- `server`: Bind and listen on UDP port

#### Serial Endpoint
```toml
[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"  # Linux/macOS
# device = "COM3"        # Windows
baud = 115200            # Common: 57600, 115200, 921600

# Optional filters (same as TCP)
```

### Message Filtering

Filters can be applied per-endpoint for fine-grained control:

- `allow_msg_id_in`: Only accept these message IDs from this endpoint
- `allow_msg_id_out`: Only send these message IDs to this endpoint
- `block_msg_id_in`: Block these message IDs from this endpoint
- `block_msg_id_out`: Do not send these message IDs to this endpoint

**Common MAVLink Message IDs**:
- 0 = HEARTBEAT
- 1 = SYS_STATUS
- 30 = ATTITUDE
- 33 = GLOBAL_POSITION_INT
- 74 = VFR_HUD
- 147 = BATTERY_STATUS

See [MAVLink message definitions](https://mavlink.io/en/messages/common.html) for complete list.

## Creating Custom Configurations

1. Copy an existing configuration file
2. Modify endpoints, ports, and filters as needed
3. Save with descriptive name (e.g., `mavrouter_custom.toml`)
4. Run with: `cargo run -- --config config/mavrouter_custom.toml`

## Common Scenarios

### Scenario 1: Simulator + GCS
Connect SITL simulator (localhost:14540) to GCS (localhost:14550):

```toml
[general]
tcp_port = 5760

[[endpoint]]
type = "udp"
address = "127.0.0.1:14540"
mode = "client"

[[endpoint]]
type = "udp"
address = "0.0.0.0:14550"
mode = "server"
```

### Scenario 2: Real Autopilot + Multiple GCS
Serial autopilot forwarded to multiple TCP clients:

```toml
[general]
tcp_port = 5760  # GCS 1, 2, 3... can all connect here

[[endpoint]]
type = "serial"
device = "/dev/ttyACM0"
baud = 57600
```

### Scenario 3: High-Bandwidth Filtering
Reduce bandwidth by only forwarding essential telemetry:

```toml
[[endpoint]]
type = "tcp"
address = "remote-gcs:5760"
mode = "client"
allow_msg_id_out = [0, 1, 30, 33, 74]  # HEARTBEAT, STATUS, ATTITUDE, POSITION, HUD
```

## Troubleshooting

### Router doesn't start
- Check TOML syntax (commas, quotes, brackets)
- Verify serial device exists: `ls /dev/tty*`
- Ensure ports are not already in use: `netstat -tulpn | grep <port>`

### No messages received
- Check endpoint modes (client vs server)
- Verify IP addresses and ports match your system
- Check firewall settings
- Review filter configurations (allow/block lists)

### Messages dropped
- Increase `bus_capacity` in `[general]`
- Check router logs for errors
- Verify network connectivity

## Default Configuration

If no config file is specified, the router looks for `mavrouter.toml` in the current directory:

```bash
cargo run  # Uses ./mavrouter.toml by default
```
