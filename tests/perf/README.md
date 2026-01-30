# Performance Tests

Performance monitoring for critical hot paths using [hotpath-rs](https://github.com/pawurb/hotpath-rs).

## Quick Start

```bash
# Run all perf tests
cargo test --features perf --release --test-threads=1 -- --ignored

# Run specific test suite
cargo test --features perf --release --test resampler -- --ignored
cargo test --features perf --release --test decoder -- --ignored
cargo test --features perf --release --test pool -- --ignored

# Run individual test
cargo test --features perf --release --test resampler perf_resampler_quality_comparison -- --ignored --nocapture
```

## Test Suites

### Resampler (`resampler.rs`)

Tests audio resampling performance across quality levels.

**Tests:**
- `perf_resampler_quality_comparison` — Compare Fast/Normal/Good/High/Maximum quality presets
- `perf_resampler_passthrough_detection` — Measure passthrough optimization (same input/output rate)
- `perf_resampler_deinterleave_overhead` — Isolate deinterleave/interleave cost

**Key metrics:**
- Time per 2048-sample chunk
- Throughput (samples/sec)
- Quality vs. performance trade-off

**Baseline targets:**
- Fast: <1ms per chunk
- Normal: <2ms
- Good: <3ms
- High: <5ms
- Maximum (FFT): <8ms

### Decoder (`decoder.rs`)

Tests Symphonia decoder performance.

**Tests:**
- `perf_decoder_wav_decode_loop` — Full decode loop performance
- `perf_decoder_probe_latency` — Format detection cold-start latency
- `perf_decoder_f32_conversion` — Sample format conversion overhead

**Key metrics:**
- Decode latency per chunk
- Probe time (cold vs. with MediaInfo)
- F32 conversion throughput

**Baseline targets:**
- WAV decode: <1ms per chunk
- Probe (cold): <200ms
- Probe (with hint): <10ms

### Pool (`pool.rs`)

Tests buffer pool lock contention and work-stealing.

**Tests:**
- `perf_pool_single_thread_get_put` — Single-threaded get/put cycle
- `perf_pool_multi_thread_contention` — 8-thread contention stress test
- `perf_pool_work_stealing` — Work-stealing behavior across shards
- `perf_pool_allocation_rate` — Fallback allocation when pool empty

**Key metrics:**
- Lock hold time
- Own-shard hit rate vs. work-steal rate
- Allocation frequency

**Baseline targets:**
- Own shard hit: >95%
- Lock hold time: <100ns
- Allocation rate: <1% of gets

## Baseline Management

### Update Baseline

After confirming an optimization improves performance:

```bash
# Run perf tests and save new baseline
cargo test --features perf --release --test-threads=1 -- --ignored
cp target/hotpath-*.json ../perf-baseline/

# Commit baseline update
git add perf-baseline/
git commit -m "perf: update baseline after [optimization description]"
```

### Compare with Baseline

```bash
# Run current tests
cargo test --features perf --release --test-threads=1 -- --ignored

# Compare with baseline (requires python script)
python ../scripts/compare_perf.py \
  --current target/hotpath-*.json \
  --baseline ../perf-baseline/hotpath-*.json \
  --threshold 10%
```

## CI Integration

Performance tests run automatically on:
- Pull requests (regression detection)
- Main branch commits (baseline tracking)

See `.github/workflows/perf.yml` for CI configuration.

## Guidelines

1. **Always use `--release`** — Debug builds produce misleading results
2. **Use `--test-threads=1`** — Avoid interference between tests
3. **Include warm-up** — First iterations include JIT/cache effects
4. **Measure in loops** — Single iterations are too noisy
5. **Isolate hot paths** — Measure one thing at a time

## Adding New Tests

1. Create test file in `tests/perf/`
2. Add `#![cfg(feature = "perf")]` at top
3. Use `#[test] #[ignore]` for perf tests
4. Use `hotpath::init()`, `enter!()`, `exit!()`, `report!()`
5. Add test target to `tests/Cargo.toml`
6. Run and establish baseline

Example:

```rust
#![cfg(feature = "perf")]

#[test]
#[ignore]
fn perf_my_hot_function() {
    use hotpath::hotpath;

    hotpath::init("my_function");

    // Warm-up
    for _ in 0..10 {
        my_hot_function();
    }

    // Measure
    hotpath::enter!("hot_loop");
    for _ in 0..1000 {
        hotpath::enter!("hot_call");
        my_hot_function();
        hotpath::exit!("hot_call");
    }
    hotpath::exit!("hot_loop");

    let report = hotpath::report!();
    println!("{}", report);
}
```

## Troubleshooting

**Error: `hotpath not found`**
→ Run with `--features perf`

**Error: `test not found`**
→ Run with `--ignored` flag

**Warning: wildly varying results**
→ Use `--release`, ensure system idle

**Question: How to measure memory?**
→ Use `dhat` or `memory-stats` crates for heap profiling
