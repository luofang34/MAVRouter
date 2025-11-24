# Performance Specifications

## Service Level Objectives (SLO)

### Routing Performance
- **Route Lookup**: < 200ns (p99)
- **Target Extraction**: < 5ns (p99)
- **Routing Update**: < 200ns (p99)
- **End-to-end Latency**: < 10µs (internal processing)

### Throughput
- **Message Routing**: > 1M msg/sec (internal bus)
- **Concurrent Operations**: > 10K msg/sec (100 concurrent TCP clients)

### Memory
- **Routing Table**: Bounded by O(systems × components). Max ~65k entries.
- **Dedup Cache**: Time-bounded (pruned every configured interval, default 0ms/disabled for low latency, or 500ms for deduplication). Memory usage is O(throughput * window).

## Benchmark Results

Run benchmarks: `cargo bench`

Representative results (Release build):
```text
routing_lookup_hit      51 ns
routing_lookup_miss     15 ns
extract_target_command   2 ns
routing_update          103 ns
routing_prune/1000      2.8 µs
```

## Performance Testing Strategy

1. **Unit Benchmarks** (`benches/routing_benchmark.rs`):
   - Micro-benchmarks for critical paths using `criterion`.
   - Run with: `cargo bench`

2. **Functional Stress Tests** (`tests/stress_test.rs`):
   - Validates functional correctness under high load (100k operations).
   - Runs in CI (Debug & Release).

3. **End-to-End Fuzzing**:
   - Validates stability against malformed data.
   - See `fuzz_test_strict.py`.

## Adding New Features

Before merging:
1. Run `cargo bench` to ensure no regressions.
2. Update this document if SLOs change.
