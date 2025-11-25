# Contributing to mavrouter-rs

Thank you for your interest in contributing to **mavrouter-rs**! We welcome contributions from the community.

## Table of Contents

- [Contributor License Agreement (CLA)](#contributor-license-agreement-cla)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Contribution Workflow](#contribution-workflow)
- [Code Standards](#code-standards)
- [Testing](#testing)
- [Commit Messages](#commit-messages)
- [Pull Request Process](#pull-request-process)

## Contributor License Agreement (CLA)

**IMPORTANT**: Before we can accept your contribution, you must agree to our [Contributor License Agreement (CLA.md)](CLA.md).

By submitting a Pull Request, you agree to the terms in `CLA.md`. Please include the following statement in your PR description:

```
I have read and agree to the CLA.md
```

### Why a CLA?

The CLA:
- Protects your rights as a contributor
- Allows the project to be dual-licensed (AGPL-3.0 for open source, proprietary for commercial use)
- Ensures all contributions can be used by the project and its users
- Provides legal clarity for patent and copyright licenses

You retain ownership of your contributions and can use them however you wish.

## Getting Started

1. **Fork the repository** on GitHub
2. **Clone your fork** locally:
   ```bash
   git clone https://github.com/YOUR-USERNAME/mavrouter-rs.git
   cd mavrouter-rs
   ```
3. **Add upstream remote**:
   ```bash
   git remote add upstream https://github.com/luofang34/mavrouter-rs.git
   ```

## Development Setup

### Prerequisites

- Rust 1.70 or later
- Python 3.8+ (for integration tests)
- `pymavlink` (install: `pip install pymavlink`)

### Build

```bash
cargo build --release
```

### Run Tests

```bash
# Rust unit tests
cargo test

# Integration tests (requires Python)
cd tests/integration
python3 verify_udp.py
python3 verify_multiclient.py
```

### Pre-release Validation

Before submitting, run the full validation:

```bash
SKIP_HW_TEST=1 ./scripts/pre_release_check.sh
```

This runs:
1. Code formatting (`cargo fmt`)
2. All tests (`cargo test`)
3. Clippy lints (`cargo clippy`)

## Contribution Workflow

1. **Create a branch** for your feature/fix:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes**

3. **Run tests** to ensure everything works

4. **Commit your changes** (see [Commit Messages](#commit-messages))

5. **Push to your fork**:
   ```bash
   git push origin feature/your-feature-name
   ```

6. **Open a Pull Request** on GitHub

## Code Standards

### Rust Code Style

- Follow Rust standard style (enforced by `rustfmt`)
- Run `cargo fmt` before committing
- No `unsafe` code (enforced by `#![forbid(unsafe_code)]`)
- All public APIs must be documented

### Code Quality

- Run `cargo clippy` and fix all warnings
- Add tests for new functionality
- Maintain or improve code coverage
- Follow existing patterns in the codebase

### Documentation

- Document all public APIs with doc comments
- Include examples in doc comments where helpful
- Update README.md if adding user-facing features

## Testing

### Unit Tests

Write unit tests for new functionality:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_your_feature() {
        // Test implementation
    }
}
```

### Integration Tests

Add integration tests in `tests/integration/` for:
- New endpoint types
- Protocol changes
- End-to-end functionality

## Commit Messages

Write clear, descriptive commit messages:

```
Short summary (50 chars or less)

Longer explanation if needed. Explain what and why, not how.

- Bullet points are okay
- Use present tense: "Add feature" not "Added feature"
- Reference issues: "Fixes #123"
```

Good examples:
- `Add UDP multicast support`
- `Fix TCP connection leak in server mode`
- `Improve routing table performance by 30%`

## Pull Request Process

1. **Ensure tests pass**: All tests must pass before merging
2. **Update documentation**: If you change APIs or add features
3. **Sign the CLA**: Include "I have read and agree to the CLA.md" in PR description
4. **Code review**: Address feedback from maintainers
5. **Squash commits**: If requested, squash commits before merging

### PR Description Template

```markdown
## Summary
Brief description of what this PR does.

## Changes
- Change 1
- Change 2

## Testing
How you tested these changes.

## CLA
I have read and agree to the CLA.md

## Related Issues
Fixes #123
```

## Areas for Contribution

Good areas to contribute:

- **Performance**: Optimize routing, reduce latency, improve throughput
- **Features**: New endpoint types, filtering capabilities, monitoring
- **Documentation**: Improve docs, add examples, tutorials
- **Testing**: Add tests, improve coverage, stress testing
- **Bug Fixes**: Fix reported issues

## Questions?

- **Issues**: Open an issue on GitHub for bugs or feature requests
- **Discussions**: Use GitHub Discussions for questions
- **Email**: Contact maintainer at luofang@luofang.org

## License

By contributing, you agree that your contributions will be licensed under the project's AGPL-3.0 license and that the project may also license them under commercial terms as specified in the CLA.

---

**Thank you for contributing to mavrouter-rs!**
