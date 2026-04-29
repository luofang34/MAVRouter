#!/usr/bin/env bash
# check-no-hardcoded-ports.sh
#
# Guardrail for CLAUDE.md line 62: "Bind ports with `127.0.0.1:0`,
# never hard-code. `#[serial]` is for shared state, not port conflicts."
#
# Scans Rust integration tests under `tests/` and Python integration
# scripts under `tests/integration/` for hard-coded TCP/UDP port
# literals. Two match forms:
#
#   URL form      — `127.0.0.1:14550`, `[::1]:5760`, etc. Shows up in
#                   TOML heredocs, mavlink connection strings, TCP
#                   stream connects.
#   Tuple form    — `('127.0.0.1', 14550)`. Shows up in Python
#                   `socket.create_connection` / `socket.bind` calls.
#
# Both require the first port digit to be `[1-9]`, so ephemeral `:0`
# and `{...}` template substitutions (`:{port}`, `:{udp_port}`) are
# naturally excluded. Python's `os.environ.get('...', 5760)` fallback
# is excluded by the same rule — the `5760` literal there has no
# `127.0.0.1` preceding it, so nothing matches.
#
# Exits non-zero (listing each offender) on any hit; silent success
# otherwise. Wired into CI's `size-budget` job so a regression lands
# a red light before merge.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Match either:
#   HOST:NNNN     (colon-delimited URL / TOML form)
#   HOST',  NNNN  (quote-close + comma + number — Python socket tuple)
# where HOST ∈ {127.0.0.1, 0.0.0.0, [::1], [::]} and NNNN starts with
# [1-9] (so `:0` and template `{…}` don't match).
pattern='(127\.0\.0\.1|0\.0\.0\.0|\[::1?\]|\[::\])([:]|["'"'"']\s*,\s*)[1-9][0-9]*'

# Rust scope: integration tests under tests/ AND unit tests under
# src/. Comment lines (//, ///, //!) are stripped before matching so
# doc examples like `//! address = "127.0.0.1:5761"` don't trigger.
#
# Some unit tests under src/ legitimately use port literals as fixture
# data — the literal never reaches a `bind()` / `connect()`, only a
# `parse::<SocketAddr>()` round-trip or a TOML deserializer assertion.
# Those paths are excluded explicitly:
#
#   src/config/tests/         — TOML parser / serde validation fixtures
#                               (no Router started, no socket bound).
#   src/endpoints/udp/tests.rs — `is_broadcast_addr` / `client_bind_addr`
#                               unit tests; `SocketAddr::parse` only,
#                               followed by predicate checks.
#   src/error.rs              — error-message Display formatting tests;
#                               port literal is just a string the error
#                               carries through `to_string()`.
#
# Anywhere else under src/ that parses a TOML config and feeds it to
# `Router::from_str` / `Router::start` IS in scope — those bind real
# sockets and must use ephemeral ports (claim helper pattern from the
# integration tests).
rust_hits=$(grep -REn --include='*.rs' "$pattern" tests src \
    | grep -vE ':[[:space:]]*//' \
    | grep -vE '^src/config/tests/' \
    | grep -vE '^src/endpoints/udp/tests\.rs:' \
    | grep -vE '^src/error\.rs:' \
    || true)

# Python scope: tests/integration/*.py. Every client-only script
# reads ports from MAVROUTER_TCP_PORT / MAVROUTER_UDP_PORT env vars
# (with 5760 / 14550 fallbacks); every self-managed script uses local
# `claim_tcp_port()` / `claim_udp_port()` helpers. Any remaining
# literal-port URL/tuple is a regression.
python_hits=$(grep -REn --include='*.py' "$pattern" tests/integration || true)

hits="$rust_hits"
if [ -n "$python_hits" ]; then
    if [ -n "$hits" ]; then
        hits="$hits
$python_hits"
    else
        hits="$python_hits"
    fi
fi

if [ -n "$hits" ]; then
    cat >&2 <<'EOF'
check-no-hardcoded-ports: hard-coded port literal(s) detected in test code.

CLAUDE.md requires tests to bind ephemeral ports via 127.0.0.1:0 and
read back the kernel-assigned port:

  - Rust: the `claim_tcp_ports` / `claim_udp_port` helpers in
    tests/integration_test.rs, tests/smart_routing_test.rs,
    tests/tlog_test.rs, and tests/network_test.rs.
  - Python: either `os.environ.get('MAVROUTER_TCP_PORT', 5760)` (for
    scripts that connect to the CI-harness-managed router) or local
    `claim_tcp_port()` / `claim_udp_port()` helpers (for self-managed
    scripts — see verify_config_dir.py, verify_endpoint_stats.py,
    verify_filtering.py, verify_tlog.py, verify_sighup_invalid.py).

Offending sites:
EOF
    echo "$hits" >&2
    exit 1
fi

echo "check-no-hardcoded-ports: OK (no hard-coded test ports detected)"
