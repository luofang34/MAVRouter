#!/usr/bin/env bash
# check-changelog.sh
#
# Guardrail: the latest released entry in CHANGELOG.md must match the
# `version` in Cargo.toml. Run from the `cargo-release` pre-release hook
# (see release.toml) so a release can't ship with a stale CHANGELOG.
#
# Rules:
#   - Cargo.toml `version = "X.Y.Z"` must have a matching `## [X.Y.Z]`
#     header in CHANGELOG.md.
#   - The `## [Unreleased]` header is allowed above it; anything else
#     between `[Unreleased]` and `[X.Y.Z]` is a desync and fails.
#
# Exits non-zero (with a diagnostic) on mismatch; prints a one-line
# confirmation on success.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cargo_toml="$PROJECT_ROOT/Cargo.toml"
changelog="$PROJECT_ROOT/CHANGELOG.md"

if [ ! -f "$cargo_toml" ]; then
    echo "check-changelog: missing $cargo_toml" >&2
    exit 1
fi
if [ ! -f "$changelog" ]; then
    echo "check-changelog: missing $changelog" >&2
    exit 1
fi

cargo_version=$(
    awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$cargo_toml"
)

if [ -z "$cargo_version" ]; then
    echo "check-changelog: could not extract version from $cargo_toml" >&2
    exit 1
fi

# First versioned header in the file, ignoring `[Unreleased]`.
changelog_version=$(
    grep -E '^## \[[0-9]+\.[0-9]+\.[0-9]+\]' "$changelog" \
        | head -n 1 \
        | sed -E 's/^## \[([0-9]+\.[0-9]+\.[0-9]+)\].*/\1/'
)

if [ -z "$changelog_version" ]; then
    echo "check-changelog: CHANGELOG.md has no versioned header yet" >&2
    exit 1
fi

if [ "$cargo_version" != "$changelog_version" ]; then
    cat >&2 <<EOF
check-changelog: version mismatch
  Cargo.toml:    $cargo_version
  CHANGELOG.md:  $changelog_version
Add a '## [$cargo_version] — YYYY-MM-DD' section to CHANGELOG.md
(or bump Cargo.toml to match) before releasing.
EOF
    exit 1
fi

echo "check-changelog: OK (v$cargo_version in both Cargo.toml and CHANGELOG.md)"
