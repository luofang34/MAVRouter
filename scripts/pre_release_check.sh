#!/usr/bin/env bash
# Pre-release validation for cargo publish
# - Runs Rust tests
# - Runs hardware tests (if connected, skippable)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

echo "========================================="
echo "Pre-Release Validation"
echo "========================================="

# 1. Rust tests
echo ""
echo "[1/3] Running Rust tests (release mode)..."
cargo test --release --all-targets --quiet
echo "✅ Rust tests passed"

# 2. Clippy
echo ""
echo "[2/3] Running clippy..."
cargo clippy --all-targets -- -D warnings
echo "✅ Clippy passed"

# 3. Hardware tests
echo ""
echo "[3/3] Hardware validation..."

# Check if user wants to skip
if [ "${SKIP_HW_TEST}" = "1" ]; then
    echo "⚠️  Hardware tests SKIPPED (SKIP_HW_TEST=1)"
    echo "   Manual validation required before publish!"
    exit 0
fi

# Check if hardware is connected
if FC_PORT=$("$SCRIPT_DIR/detect_fc_serial.sh" 2>/dev/null); then
    echo "✅ Flight controller detected: $FC_PORT"
    echo ""
    echo "Running hardware tests..."
    echo "(Set SKIP_HW_TEST=1 to skip)"
    echo ""

    # Run full hardware validation
    if "$SCRIPT_DIR/full_hw_validation.sh"; then
        echo ""
        echo "✅ Hardware tests passed"
    else
        echo ""
        echo "❌ Hardware tests failed"
        cat router_hw_val.log # Add this line to show the log
        echo ""
        echo "To skip hardware tests and retry:"
        echo "  SKIP_HW_TEST=1 cargo release --dry-run"
        exit 1
    fi
else
    echo "⚠️  No flight controller detected"
    echo ""
    echo "Hardware tests are recommended before release."
    echo ""
    echo "Options:"
    echo "  1. Connect flight controller and retry"
    echo "  2. Skip tests: SKIP_HW_TEST=1 cargo release ..."
    echo ""

    # In non-interactive mode, fail
    if [ ! -t 0 ]; then
        echo "❌ Non-interactive mode: hardware required or use SKIP_HW_TEST=1"
        exit 1
    fi

    # Ask user
    read -p "Continue without hardware tests? (yes/no): " choice
    case "$choice" in
        yes|y|Y)
            echo "⚠️  Proceeding without hardware validation"
            ;;
        *)
            echo "Aborted. Connect hardware and retry."
            exit 1
            ;;
    esac
fi

echo ""
echo "========================================="
echo "✅ Pre-release validation complete"
echo "========================================="
