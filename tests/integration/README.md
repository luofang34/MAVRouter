# Integration Tests

This directory contains **Python-based end-to-end tests** for the MAVLink Router. These complement the Rust integration tests in `tests/*.rs`.

## Testing Strategy: Hybrid Approach

The project uses a **two-tier testing strategy**:

### 🦀 Rust Tests (`tests/*.rs`)
- **Purpose**: Unit and integration tests for internal logic
- **Location**: `tests/router_integration.rs`, `tests/message_filtering.rs`
- **Advantages**:
  - No external dependencies (Python/pip)
  - Fast compilation and execution
  - Direct access to internal modules
  - Type-safe, compile-time checks
- **Run**: `cargo test`

### 🐍 Python Tests (`tests/integration/*.py`)
- **Purpose**: End-to-end "black box" tests from client perspective
- **Location**: This directory
- **Advantages**:
  - Tests from external client viewpoint (like real GCS)
  - Uses mature pymavlink library
  - Simulates real-world MAVLink clients
  - Chaos engineering and fuzzing scenarios
- **Run**: See instructions below

### Why Both?

- **Rust tests**: Verify internal correctness, fast feedback during development
- **Python tests**: Verify system behavior from user/client perspective, catch integration issues

## Prerequisites

Install required Python packages:

```bash
pip install pymavlink
```

## Test Categories

### 1. Integration Tests (Functional Verification)

These tests verify correct MAVLink communication and protocol handling:

#### `verify_hardware.py`
- **Purpose**: Verify hardware/autopilot connection
- **What it tests**: Connects to router and waits for heartbeat from PX4/autopilot
- **Usage**: `python tests/integration/verify_hardware.py`
- **Expected result**: Receives heartbeat with correct system ID and component ID

#### `verify_tx.py`
- **Purpose**: Verify bidirectional communication
- **What it tests**: Sends MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES and waits for response
- **Usage**: `python tests/integration/verify_tx.py`
- **Expected result**: Receives AUTOPILOT_VERSION or COMMAND_ACK message

#### `verify_udp.py`
- **Purpose**: Verify UDP endpoint functionality
- **What it tests**: Sends heartbeat via UDP and waits for response
- **Usage**: `python tests/integration/verify_udp.py`
- **Expected result**: Receives heartbeat from system ID 1

#### `verify_params.py`
- **Purpose**: Verify parameter read/write operations
- **What it tests**: Reads parameter at index 0, writes same value back
- **Usage**: `python tests/integration/verify_params.py`
- **Expected result**: Receives parameter value and write confirmation

### 2. Stress Tests

These tests verify router performance under high load:

#### `stress_test.py`
- **Purpose**: High-frequency message throughput test
- **What it tests**: Sends 1000 PING messages rapidly
- **Usage**: `python tests/integration/stress_test.py`
- **Expected result**: Router handles high message rate without crashes
- **Metrics**: Measures messages/second throughput

### 3. Chaos Engineering Tests

These tests verify router resilience and error handling:

#### `chaos_test.py`
- **Purpose**: Multiple chaos engineering scenarios
- **What it tests**: Three sub-tests:
  - **Slow Loris**: Sends data extremely slowly (1 byte per 100ms)
  - **Buffer Overflow**: Sends 1.1MB of garbage data
  - **FD Exhaustion**: Opens 200 concurrent connections
- **Usage**: `python tests/integration/chaos_test.py`
- **Expected result**: Router remains stable and responsive

### 4. Security/Fuzzing Tests

These tests verify router security and robustness:

#### `fuzz_test.py`
- **Purpose**: Basic fuzzing test
- **What it tests**: Sends 1MB of random data to TCP interface
- **Usage**: `python tests/integration/fuzz_test.py`
- **Expected result**: Router does not crash

#### `fuzz_test_strict.py`
- **Purpose**: Strict fuzzing test (improved version)
- **What it tests**: Sends random data, then verifies router can still process valid MAVLink
- **Usage**: `python tests/integration/fuzz_test_strict.py`
- **Expected result**: Router handles invalid data gracefully and continues normal operation

## Running Tests

### Run all integration tests:
```bash
# Start the router first
cargo run -- --config config/mavrouter_test.toml

# In another terminal, run tests
for test in tests/integration/verify_*.py; do
    echo "Running $test..."
    python "$test"
done
```

### Run stress tests:
```bash
python tests/integration/stress_test.py
python tests/integration/chaos_test.py
```

### Run security tests:
```bash
python tests/integration/fuzz_test.py
python tests/integration/fuzz_test_strict.py
```

## Test Configuration

Most tests connect to:
- **TCP**: `localhost:5760` (router's implicit TCP server)
- **UDP**: `localhost:14550` (router's UDP client endpoint)

Ensure your router configuration matches the test expectations, or modify the test connection parameters accordingly.

## Expected Router Configuration

For tests to work properly, ensure your `mavrouter.toml` includes:

```toml
[general]
tcp_port = 5760  # Implicit TCP server

[[endpoint]]
type = "udp"
address = "127.0.0.1:14550"
mode = "client"
```

## Interpreting Results

- **Green/Success**: Test passed, router behaves correctly
- **Red/Error**: Test failed, investigate router logs
- **Timeout**: Router may not be running or not configured correctly

## Adding New Tests

When adding new integration tests:

1. Name file descriptively: `verify_<feature>.py` or `test_<scenario>.py`
2. Include clear docstring explaining test purpose
3. Add entry to this README
4. Use consistent connection parameters
5. Include proper error handling and timeouts
6. Print clear success/failure messages

## Test Statistics

| Category | Test Count | Total Lines |
|----------|------------|-------------|
| Integration | 4 | 212 |
| Stress | 1 | 36 |
| Chaos | 1 (3 sub-tests) | 87 |
| Security | 2 | 85 |
| **Total** | **8** | **420** |
