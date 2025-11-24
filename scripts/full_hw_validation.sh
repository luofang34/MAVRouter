#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Ensure release build
echo "Building release..."
cargo build --release --quiet

echo "Starting Router for Full Hardware Validation..."
# Kill any existing instances
pkill -f mavrouter-rs || true

# Config check
if [ ! -f "config/mavrouter_test.toml" ]; then
    echo "Error: config/mavrouter_test.toml not found."
    exit 1
fi

# Start Router
RUST_LOG=info ./target/release/mavrouter-rs --config config/mavrouter_test.toml > router_hw_val.log 2>&1 &
ROUTER_PID=$!

# Cleanup trap
cleanup() {
    kill $ROUTER_PID 2>/dev/null || true
}
trap cleanup EXIT

# Wait for startup
sleep 2

# Verify Process is running
if ! kill -0 $ROUTER_PID 2>/dev/null; then
    echo "Router failed to start. Check router_hw_val.log"
    cat router_hw_val.log
    exit 1
fi

echo "=== Functional Tests ==="
echo "[1/7] Basic Connection (Heartbeat)..."
python3 tests/integration/verify_hardware.py

echo "[2/7] Command Roundtrip (TCP <-> Serial)..."
python3 tests/integration/verify_tx.py

echo "[3/7] UDP Broadcast (UDP <-> Serial)..."
python3 tests/integration/verify_udp.py

echo "[4/7] Parameter Operations (Read/Write)..."
python3 tests/integration/verify_params.py

echo "=== Stress & Resilience Tests ==="
echo "[5/7] Ping Storm (Throughput)..."
python3 tests/integration/stress_test.py

echo "[6/7] Fuzzing (Malicious Payload)..."
python3 tests/integration/fuzz_test_strict.py

echo "[7/7] Chaos (Slow Loris, FD Exhaustion)..."
python3 tests/integration/chaos_test.py

echo "========================================"
echo "✅ All Hardware Tests Passed."
echo "========================================"
