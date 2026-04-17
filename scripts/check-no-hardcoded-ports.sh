#!/usr/bin/env bash
# check-no-hardcoded-ports.sh
#
# Guardrail for CLAUDE.md line 62: "Bind ports with `127.0.0.1:0`,
# never hard-code. `#[serial]` is for shared state, not port conflicts."
#
# Scans Rust test files under `tests/` and `src/**/tests*.rs` for
# hard-coded TCP/UDP port literals like `127.0.0.1:14550` or
# `[::1]:5760`. Ephemeral ports (`:0`) and formatted templates
# (`:{port}`, `:{udp_port}`) are allowed — the latter is how the
# `claim_*_ports` helpers feed their kernel-assigned ports into TOML.
#
# Exits non-zero (listing each offender) on any hit; silent success
# otherwise. Wire this into CI next to fmt/clippy/test so a regression
# lands a red light before the merge.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Matches:
#   "127.0.0.1:12345"   (double-quoted literal)
#   '127.0.0.1:12345'   (single-quoted literal)
#   [::1]:12345         IPv6 loopback variant
#   0.0.0.0:12345       wildcard bind
# Excludes `:0` (ephemeral) by requiring `[1-9]` as the first port digit.
pattern='(127\.0\.0\.1|0\.0\.0\.0|\[::1?\]|\[::\]):[1-9][0-9]*'

# Scope: top-level integration tests only. These are the files that
# actually bind sockets against the tokio runtime — unit tests under
# src/ either (a) use port literals as TOML/SocketAddr *fixture data*
# for parser assertions (e.g. src/config/tests/*.rs) or (b) already
# bind on `:0` (src/endpoints/udp/tests.rs). Production code is out of
# scope — example configs legitimately reference well-known defaults
# like 14550/5760.
#
# Use `grep -RE` directly (not the sandbox Grep tool) so the script is
# portable to CI and the pre-push hook without depending on rg.
hits=$(grep -REn --include='*.rs' "$pattern" tests || true)

if [ -n "$hits" ]; then
    cat >&2 <<EOF
check-no-hardcoded-ports: hard-coded port literal(s) detected in test code.

CLAUDE.md requires tests to bind ephemeral ports via 127.0.0.1:0 and
read back the kernel-assigned port (see the \`claim_tcp_ports\` /
\`claim_udp_port\` helpers already used in tests/integration_test.rs,
tests/smart_routing_test.rs, tests/tlog_test.rs, and tests/network_test.rs).

Offending sites:
EOF
    echo "$hits" >&2
    exit 1
fi

echo "check-no-hardcoded-ports: OK (no hard-coded test ports detected)"
