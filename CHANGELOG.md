# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `--config-dir` / `MAVROUTER_CONF_DIR`: point at a directory of `*.toml`
  fragments that are merged alphabetically onto the main config.
- `Router::stop` now runs under a bounded shutdown budget, enumerates any
  task that fails to exit in time, logs it at `error!`, and aborts it so
  `stop()` is guaranteed to return.
- Dedicated routing-updater task: all routing-table writes go through an
  mpsc channel consumed by a single `spawn_routing_updater`, so async
  endpoint ingress never holds a `RwLock::write()` on a tokio worker.
- CI size-budget job: hard-fails on any `src/**/*.rs` file over 500 lines;
  reports functions over 80 lines as warnings.
- CI `await_holding_lock = "deny"`, `let_underscore_must_use = "deny"`,
  `let_underscore_future = "deny"`, and a `disallowed_types` entry forbidding
  `anyhow::Error` in crate code.
- `cargo audit` workflow gating every PR plus a Monday cron, so RustSec
  advisories on the dep tree surface as a CI red light instead of after
  release.
- MSRV verification job pinned to 1.75 — `cargo check --all-targets
  --locked` against the declared `rust-version`.
- crates.io trusted-publishing release pipeline (`.github/workflows/
  publish.yml`): OIDC token exchange (no stored `CARGO_REGISTRY_TOKEN`),
  tag-version match check, `workflow_dispatch` dry-run that exercises
  the full OIDC path before a real tag is cut.
- Dependabot weekly grouped PRs for the `github-actions` ecosystem.
- CI guardrail against stray Finder-style `* N.*` duplicate files.
- `pipeline_benchmark`: end-to-end criterion bench covering ingress
  parsing, routing-table lookup, and broadcast fan-out under contention.
- Test-only `RunEvent` enum and `SignalSource` trait (extracted reload
  loop in `src/run_loop.rs`) so SIGHUP-reload semantics are unit-testable
  without forking the binary.
- Shared Python test helpers (`tests/integration/_test_helpers.py`):
  bounded `wait_for_tcp` / `drain_recv_match` / `collect_until` —
  every blocking op now has a wall-clock deadline so a wedged router
  fails fast instead of hanging until the CI job timeout.

### Changed
- Narrowed the public API down to `Router`, `config::*`, `error::*`, and the
  supporting types reachable through `Router`'s accessor signatures. All
  other modules are `pub(crate)`. A dev-only `_internal` feature exposes
  what benches need; `tests/public_api_test.rs` pins the intended surface.
- Migrated every integration test that reached into internals into an
  in-crate `#[cfg(test)] mod tests` submodule. The `tests/` directory now
  contains only tests that drive the router through `Router::from_str`.
- `config.rs` split into per-concern submodules (`general`, `endpoint`,
  `defaults`, `merge`, `validate`, plus a tests subtree). No file in the
  crate exceeds the 500-line budget.
- **MSRV bumped 1.70 → 1.75** to enable async-fn-in-trait for the extracted
  `SignalSource` seam.
- `routing.rs` God-Module split into per-concern submodules (`table`,
  `shard`, `groups`, `stats`, `update`). The hot egress path now snapshots
  the group config lock-free via `ArcSwap` and never holds two locks
  simultaneously, eliminating an inverted-ordering deadlock against
  `is_sniffer_endpoint`.
- `endpoints/` ingress pipeline split into named stages (filter / dedup /
  observe / publish) and the per-protocol spawn API unified.
- `endpoint_core.rs`: `EndpointStats`, `EndpointStatsSnapshot`, and
  `ExponentialBackoff` extracted into `endpoint_core/{stats,backoff}.rs`;
  `tests.rs` partitioned into per-concern submodules (`backoff`, `stats`,
  `timestamp`, `incoming`, `outgoing`, `loopback`, `lagged`, `helpers`).
- `main.rs` reload loop extracted into `run_loop::run_with_signals` behind
  a `SignalSource` trait. `main.rs` shrinks from ~290 to ~60 lines.
- `RouterError` thickened to preserve TOML span/line context through the
  config-parse error chain.
- Stats Reporter and Stats Socket spawn moved from `main.rs` into
  `orchestration` so the binary stays focused on signal wiring.
- Integration tests (`integration_test.rs`, `smart_routing_test.rs`,
  `network_test.rs`) replace blind `tokio::time::sleep` with event-sync
  primitives: `wait_for_bus_subscribers` (polls broadcast receiver count),
  `connect_tcp_with_retry` (closes the bind/release race), and
  `wait_for_routing_systems` (polls `RoutingTable::stats`).
- Hard-coded test ports replaced with ephemeral `127.0.0.1:0` reservations
  across the Rust integration tests, the Python integration suite, and
  unit tests inside `src/`. The `check-no-hardcoded-ports.sh` guardrail
  now scans `src/` too (with explicit fixture-file allowlists).
- `let _ = ...` silent-error sites across TCP accept / stats socket
  cleanup / shutdown timeout / bench infrastructure either handle the
  error explicitly or surface it through `.ok()` / `.inspect_err`.
- `Router::stop`: dropped a 500ms pre-shutdown sleep that was a workaround
  for a now-fixed race.
- Endpoint supervisor: `tokio_serial::SerialStream::open_native_async`
  is bounded by a 3s timeout so a stuck device file can't hang the
  supervisor restart loop indefinitely.
- Bus broadcast `RecvError::Lagged(n)`: counted into `EndpointStats::
  bus_lagged`, surfaced in stats snapshots, and logged at `error!` for
  the UDP sender and TLog logger paths.
- All Node-based GitHub Actions on Node 24 runtime: `actions/checkout`
  v4 → v6, `actions/setup-python` v5 → v6, `actions/upload-artifact`
  v4 → v7, `codecov/codecov-action` v5 → v6.
- Test reliability: dropped all `#[serial]` attributes (replaced by
  ephemeral-port discipline) and removed `serial_test` as a dev-dep.

### Removed
- `From<anyhow::Error> for RouterError` and the unused `Internal(String)`
  variant. `anyhow::Error` is now disallowed at the Clippy level; errors
  use the structured `RouterError` variants with explicit `.map_err()`.
- `pub use parking_lot::RwLock` and `pub use tokio_util::sync::CancellationToken`
  from the crate root. Downstream users depend on those crates directly,
  or use `Router::cancel_token()`.
- Unsupported "50 ns / 34 kHz" performance claims from crate docs and
  CHANGELOG. Benches still live in `benches/` but their absolute numbers
  are not advertised.

### Fixed
- `let _ = …` silent-error sites across TCP accept / stats socket cleanup /
  shutdown timeout. Each site now either handles the error explicitly or
  surfaces it through `.ok()` / `.inspect_err`.
- Routing hot egress path: previously could hold a shard read lock and
  the group-config lock simultaneously, with inverted ordering against
  `is_sniffer_endpoint`. Refactored to snapshot config first
  (lock-free), then take at most one shard lock.

### Security
- **RUSTSEC-2026-0007** (`bytes` `BytesMut::reserve` integer overflow):
  `Cargo.lock` bumped to `bytes 1.11.1`.
- **RUSTSEC-2017-0008** (`serial` crate unmaintained): allowlisted in
  `.cargo/audit.toml` with rationale. The unmaintained `serial 0.4.0`
  reaches the lockfile only as a transitive dep of `mavlink 0.12`;
  mavrouter uses `tokio-serial` for actual serial I/O, so the
  unmaintained crate is not on any code path we exercise.

## [0.1.5] — 2026-02-09

### Added
- Shared `orchestration` module factors endpoint spawning across the binary
  and the library; `Router::{from_str, from_file}` let downstream crates
  embed the router with a single call.
- Endpoint groups: endpoints sharing a `group` label share routing
  knowledge, so redundant physical links to the same vehicle forward
  together.
- Sniffer sysids: endpoints observing a listed system id receive all
  traffic unconditionally, independent of per-target routing.
- Expanded test coverage for `EndpointCore`, TLog, and the network
  error/retry paths.

### Changed
- Message bus migrated to `tokio::broadcast` for lower-latency fan-out.
- Filter lookups use `ahash` for faster `HashSet` hits on the egress path.
- Stress-test thresholds tuned for GitHub Actions runners.

### Fixed
- Stronger error typing throughout: `RouterError` replaces `anyhow::Error`
  in library code; routing-table and endpoint paths propagate structured
  errors with per-call context.
- Endpoint supervisor applies exponential backoff correctly across
  restarts and resets after sustained stable operation.

## [0.1.4] — 2025-11-27

### Added
- SIGHUP configuration reload on Unix; SIGTERM graceful shutdown.
- Windows signal handling (Ctrl+C only; SIGHUP/SIGTERM are no-ops on
  non-Unix platforms).
- Integration tests covering SIGHUP reload and supervised restart.

### Changed
- Filter precedence: block lists strictly override allow lists when an id
  appears in both.
- Stress-test thresholds lowered to match GitHub Actions hardware.

### Fixed
- Intermittent timing issues on slower runners.
- Bus/write ordering bugs surfaced by the expanded stress test.

## [0.1.3] — 2025-11-25

### Changed
- Crate renamed to `mavrouter` to match the published crates.io name. No
  code changes; republished so the package namespace matches the binary.

## [0.1.1] — 2025-11-25

### Added
- Intelligent routing with automatic topology learning — `RoutingTable`
  plus `needs_update_for_endpoint` for fast-path dedup.
- Routing benchmarks under `benches/routing_benchmark.rs`.
- `memchr` STX search in the framing parser.
- `DashMap`-based UDP client tracking for better concurrency.
- Parallel UDP broadcast delivery.
- `Config::validate` and a runnable `examples/simple_router.rs`.
- `EndpointCore` abstraction unifying TCP, UDP, and Serial ingress paths.
- Real integration tests for the network endpoints.
- Adaptive stress-test iteration count (scales with the runner's core
  count).

### Changed
- Licensed under AGPL-3.0.
- Bus restored to non-blocking for throughput.
- Async-broadcast bus + writer path tweaks.
- `ahash` used in the dedup path for faster hashing.

## [0.1.0] — 2025-11-24

### Added
- Initial release.
- Serial, UDP, and TCP endpoints with client/server modes.
- Per-endpoint message filtering (allow/block by msg_id, src_sys_id,
  src_comp_id, src_comp_out).
- Time-based message deduplication.
- TLog recording.
- Graceful shutdown with log integrity.
- Comprehensive test suite and performance benchmarks.

[Unreleased]: https://github.com/luofang34/MAVRouter/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/luofang34/MAVRouter/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/luofang34/MAVRouter/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/luofang34/MAVRouter/compare/v0.1.1...v0.1.3
[0.1.1]: https://github.com/luofang34/MAVRouter/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/luofang34/MAVRouter/releases/tag/v0.1.0

