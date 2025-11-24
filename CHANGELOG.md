# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2025-11-24

### Added
- Initial release
- Intelligent MAVLink routing with automatic topology learning
- Support for Serial, UDP, TCP endpoints (client/server modes)
- Message filtering (allow/block lists per endpoint)
- Time-based message deduplication
- High-throughput TLog recording
- Graceful shutdown with log integrity
- Comprehensive test suite (20+ tests)
- Performance benchmarks

### Features
- **Smart Routing**: Routes messages only to endpoints that need them
- **Loop Prevention**: Automatic detection and prevention of routing loops
- **Dynamic Topology**: No manual configuration, learns routes automatically
- **High Performance**: 50ns routing decisions, tested at 10k msg/sec
- **Zero Unsafe Code**: 100% safe Rust
- **Production Ready**: Comprehensive error handling, no unwraps

### Performance
- Route lookup: ~50ns
- Target extraction: ~2ns
- Message routing: <1µs end-to-end
- Tested throughput: 10,000 messages/second sustained

[0.1.0]: https://github.com/luofang34/mavrouter-rs/releases/tag/v0.1.0
