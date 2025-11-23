# CI/CD Workflows

This directory contains GitHub Actions workflows for continuous integration and testing.

## 🔄 Workflows

### Main CI Pipeline (`ci.yml`)

Runs on every push to `main` and on all pull requests.

#### Jobs

##### 1. **Check** ✅
- **Purpose**: Fast compilation check
- **Runs**: `cargo check --verbose`
- **Duration**: ~30 seconds
- **When it fails**: Code doesn't compile

##### 2. **Rustfmt** 📝
- **Purpose**: Enforce code formatting standards
- **Runs**: `cargo fmt --check`
- **Duration**: ~10 seconds
- **When it fails**: Code is not formatted correctly
- **Fix**: Run `cargo fmt` locally

##### 3. **Clippy** 🔍
- **Purpose**: Lint code for common mistakes and style issues
- **Runs**: `cargo clippy -- -D warnings`
- **Duration**: ~1 minute
- **When it fails**: Clippy warnings detected
- **Fix**: Run `cargo clippy --fix` locally

##### 4. **Test** 🧪
- **Purpose**: Run all Rust tests
- **Runs**:
  - Unit tests: `cargo test --lib --verbose`
  - Integration tests: `cargo test --test router_integration --test message_filtering --verbose`
  - Doc tests: `cargo test --doc --verbose`
- **Duration**: ~2 minutes
- **When it fails**: Test assertions fail or code panics

##### 5. **Benchmark** ⚡
- **Purpose**: Ensure benchmarks compile
- **Runs**: `cargo bench --features benchmarks --no-run`
- **Duration**: ~1 minute
- **When it fails**: Benchmark code doesn't compile
- **Note**: Does not run actual benchmarks in CI (too time-consuming)

##### 6. **Python Integration Tests** 🐍
- **Purpose**: Run end-to-end tests from external client perspective
- **Setup**:
  - Installs Python 3.11 and pymavlink
  - Builds router in release mode
  - Starts router in background with test configuration
- **Runs**:
  - `verify_udp.py` - UDP communication test
  - `stress_test.py` - High-frequency message throughput
  - `chaos_test.py` - Chaos engineering scenarios
- **Duration**: ~3 minutes
- **When it fails**: Router crashes or fails to handle messages correctly
- **Artifacts**: Uploads router logs for debugging

##### 7. **Code Coverage** 📊
- **Purpose**: Generate test coverage reports
- **Runs**: `cargo llvm-cov --all-features --workspace`
- **Output**: `lcov.info` coverage file
- **Integration**: Uploads to Codecov (requires `CODECOV_TOKEN` secret)
- **Duration**: ~2 minutes
- **Artifacts**: Coverage report available as artifact

---

## 📊 CI Status Matrix

| Job | Fast Fail? | Blocks PR? | Runs On |
|-----|-----------|-----------|---------|
| Check | ✅ Yes | ✅ Yes | All branches |
| Rustfmt | ✅ Yes | ✅ Yes | All branches |
| Clippy | ⚠️ Medium | ✅ Yes | All branches |
| Test | ❌ No | ✅ Yes | All branches |
| Benchmark | ⚠️ Medium | ⚠️ Optional | All branches |
| Python Tests | ❌ No | ⚠️ Optional | All branches |
| Coverage | ❌ No | ❌ No | All branches |

**Legend:**
- **Fast Fail**: Fails quickly if there's an issue
- **Blocks PR**: Must pass before PR can be merged
- **Optional**: Failure doesn't prevent merging

---

## 🚀 Running CI Locally

### Run all checks before pushing:

```bash
# 1. Check compilation
cargo check --verbose

# 2. Format code
cargo fmt

# 3. Run linter
cargo clippy -- -D warnings

# 4. Run all tests
cargo test --verbose

# 5. Check benchmarks compile
cargo bench --features benchmarks --no-run

# 6. (Optional) Run Python tests
./target/release/mavrouter-rs --config config/mavrouter.toml &
ROUTER_PID=$!
sleep 2
python tests/integration/verify_udp.py
python tests/integration/stress_test.py
kill $ROUTER_PID
```

### Quick pre-commit check:

```bash
cargo fmt && cargo clippy && cargo test
```

---

## 🔧 Configuration

### Required Secrets

- **`CODECOV_TOKEN`** (optional): Token for uploading coverage to Codecov
  - Get from: https://codecov.io/
  - Add to: Repository Settings → Secrets → Actions

### Modifying CI

When adding new tests or checks:

1. **Add new Rust tests**: Automatically picked up by `cargo test`
2. **Add new Python tests**: Add to `python-tests` job in `ci.yml`
3. **Add new benchmark**: Automatically picked up by `cargo bench`
4. **Change test timeout**: Modify job-level `timeout-minutes` (default: 360)

---

## 📈 Performance Targets

| Metric | Target | Current |
|--------|--------|---------|
| Total CI Duration | < 10 minutes | ~8 minutes |
| Test Pass Rate | > 99% | ✅ |
| Code Coverage | > 70% | 📊 Check Codecov |
| Clippy Warnings | 0 | ✅ |

---

## 🐛 Troubleshooting

### CI fails but works locally?

**Check:**
1. **Different Rust version**: CI uses stable, check with `rustup default stable`
2. **Cached dependencies**: CI has clean build, try `cargo clean` locally
3. **Platform differences**: CI runs on Ubuntu, might behave differently on macOS/Windows

### Python tests fail in CI?

**Common causes:**
1. **Port conflicts**: Router might fail to bind to port 5760
2. **Timing issues**: Increase sleep duration in CI config
3. **Missing hardware**: Some tests require actual autopilot hardware (expected to skip in CI)

### Coverage report empty?

**Check:**
1. **Tests actually ran**: Look for test execution in CI logs
2. **Feature flags**: Ensure `--all-features` includes your code
3. **Codecov token**: Verify `CODECOV_TOKEN` secret is set

---

## 📝 Adding New Workflows

### Example: Add a release workflow

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build release
        run: cargo build --release
      - name: Upload binary
        uses: actions/upload-artifact@v4
        with:
          name: mavrouter-rs-linux
          path: target/release/mavrouter-rs
```

---

## 🎯 Best Practices

1. **Keep CI fast**: Each job should complete in < 5 minutes
2. **Fail fast**: Put fast checks (fmt, clippy) before slow ones (tests)
3. **Use caching**: GitHub Actions caches Cargo dependencies automatically
4. **Parallel jobs**: Independent jobs run in parallel (already configured)
5. **Artifacts**: Upload logs and binaries for debugging failed runs
6. **Optional jobs**: Don't block PRs on flaky or slow tests

---

## 🔗 Related Documentation

- [GitHub Actions Documentation](https://docs.github.com/en/actions)
- [Rust CI Best Practices](https://github.com/actions-rs/meta/blob/master/recipes/quickstart.md)
- [Codecov Documentation](https://docs.codecov.com/)
