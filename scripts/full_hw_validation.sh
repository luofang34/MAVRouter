#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Ensure release build
cargo build --release --quiet

echo "Starting Router for Hardware Validation..."
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

echo "[1/3] Verifying Command/Response (TCP <-> Serial)..."
python3 tests/integration/verify_tx.py

echo "[2/3] Verifying Heartbeat Broadcast (UDP <-> Serial)..."
python3 tests/integration/verify_udp.py

echo "[3/3] Verifying Resilience (Fuzzing)..."
python3 tests/integration/fuzz_test_strict.py

echo "All Hardware Tests Passed."
