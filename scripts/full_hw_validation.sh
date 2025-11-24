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
    echo "Stopping Router (PID: $ROUTER_PID)..."
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

run_test() {
    local description="$1"
    local script="$2"
    local timeout_sec="$3"
    local allow_fail="$4"

    echo "--------------------------------------------------"
    echo "Running: $description"
    echo "Script: $script"
    echo "Timeout: ${timeout_sec}s"
    
    if timeout "$timeout_sec" python3 "$script"; then
        echo "✅ PASSED: $description"
    else
        local exit_code=$?
        if [ $exit_code -eq 124 ]; then
             echo "❌ FAILED: $description (TIMED OUT)"
        else
             echo "❌ FAILED: $description (Exit Code: $exit_code)"
        fi

        if [ "$allow_fail" = "true" ]; then
            echo "⚠️  WARNING: Optional test failed. Continuing..."
        else
            echo "🚨 CRITICAL TEST FAILED. Aborting."
            cat router_hw_val.log
            exit 1
        fi
    fi
}

echo "=== Tier 1: Critical Functional Tests (Must Pass) ==="
run_test "Basic Connection (Heartbeat)" "tests/integration/verify_hardware.py" 30 false
run_test "Command Roundtrip (TCP <-> Serial)" "tests/integration/verify_tx.py" 30 false
run_test "UDP Broadcast (UDP <-> Serial)" "tests/integration/verify_udp.py" 30 false

echo "=== Tier 2: Important Validation (Should Pass) ==="
run_test "Parameter Operations (Read/Write)" "tests/integration/verify_params.py" 45 false
run_test "Ping Storm (Throughput)" "tests/integration/stress_test.py" 60 false

echo "=== Tier 3: Resilience & Chaos (Allowed to Fail) ==="
# Chaos and Fuzz tests are allowed to fail or timeout without breaking the build
run_test "Fuzzing (Malicious Payload)" "tests/integration/fuzz_test_strict.py" 60 true
run_test "Chaos (Slow Loris, FD Exhaustion)" "tests/integration/chaos_test.py" 120 true

echo "========================================"
echo "✅ All Critical Hardware Tests Passed."
echo "========================================"