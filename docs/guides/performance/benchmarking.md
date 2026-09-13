# Benchmarking & methodology

*Tier legend and the rest of the family: [performance.md](../performance.md).*

No lint enforces a measurement, so every entry here is manual and *tier: bench*.

## Measure the artifact you ship

**Benchmark input by value, or a result the optimizer deletes.** Pass the borrow
through `black_box` and return the output: a by-value input memcpys on every
iteration, and a discarded result can be optimized away entirely.
*present in kithara: tests/benches/perf_audit.rs already black-boxes the borrow.*

**Setup and drop timed inside the iteration.** A clone and its `Drop` inside the
timed closure are counted as work. Use criterion's batched-ref form to exclude
both, and `iter_with_large_drop` for large outputs; never
`BatchSize::PerIteration` for a sub-microsecond kernel. *partial in kithara: the
stretch bench excludes setup, not drop.*

**Flamegraph of a stripped release.** The release profile ships stripped symbols
at opt-level "z", so stacks come back truncated. Profile from a dedicated
profile that inherits release with debug line tables and no strip - line tables
alone do not change codegen. On macOS use samply or Instruments, both
inline-frame aware. *gap in kithara: no profiling profile is defined.*

**Benchmarking a binary nobody ships.** The bench-versus-release profile mismatch
(opt-3, LTO off, many codegen units against the shipped opt-z, fat LTO, one unit)
is owned by [dispatch-build.md](dispatch-build.md).

**Wall-time PR gate on shared CI.** A shared runner cannot resolve a wall-time
delta. Keep criterion wall-time opt-in and local, and hard-gate only on bare
metal or on one-shot instruction counts. Already decided in kithara: benchmark
execution is skipped unless `RUN_BENCHMARKS=1` is set (`.config/just/perf.just`).

**Trusting a small delta.** Treat any wall-time delta below roughly twice the
measured noise floor as unproven: corroborate with repeated runs or with
layout-insensitive counts, and set criterion's noise threshold when gating.

**Instruction counts without a cache model.** Instruction count alone misjudges a
memory-bound kernel, whose cost is stalls. Latent here - kithara measures
criterion wall time only, with no callgrind lane.

**Truncated sampling.** Cut the *number* of benches, not the sample size.
kithara's `sample_size(20)` is a deliberate opt-in-speed tradeoff, so a detector
for a small sample size would only flag its own choice.

**Hot-cache illusion.** A warm-L1 fixture overstates headroom: rotate a working
set larger than last-level cache and validate the ranking on the real pipeline.
See [test-harness.md](../test-harness.md).

## Measuring the real-time path

**A mean hides xruns** (I3). Assert the worst-case per-callback duration and make
underrun counters first-class test output; never gate a DSP kernel on a criterion
mean or median. Pair the timing with the rtsan and no_block lanes so blocking is
caught even when the timing passes. The discipline itself lives in
[realtime.md](realtime.md).

**A sampling profiler perturbs the RT thread** (I10). For Apple Silicon benches,
set explicit QoS on the benched threads, reach thermal steady state on the same
machine and power source, corroborate with instruction counts, and verify
separately that the shipping callback runs at RT priority.

**Ring capacity chosen by a round number.** Size an SPSC ring by burst tolerance
plus latency budget and measure it; the "fit it in L1" folklore is refuted. Rings
exist in kithara-audio and kithara-play.

## Where the rest of the rules live

A benchmark must not talk you out of an invariant. Each of these is already an
error-level gate or owned by another guide; this file only points at the owner.

- `Arc<Mutex<Collection>>`, god-maps, `Arc<Atomic*>` as glue - AGENTS.md red-flag
  gate; `arch.no-arc-mutex-collection`, `arch.no-arc-mutex-godmap`.
- Fallback chains and sentinel values - AGENTS.md; `rust.no-fallback-*`,
  `rust.no-sentinel-*`.
- Direct time, sleep, or rng instead of the platform - `arch.no-direct-time`,
  `arch.no-implicit-sleep`, `arch.no-implicit-rng`.
- `unwrap()` / `expect()` in production - AGENTS.md; `clippy::unwrap_used`=deny,
  `rust.no-expect-bare-string`.
- Pool construction outside its composition owner, per-packet and per-segment
  allocation, `Bytes` payload sharing, `Arc` as ownership glue, allocator swaps -
  [allocation.md](allocation.md).
- RT-callback discipline (no async primitive, tracing, channel allocation, or
  last-`Arc` drop on the callback), the pool-miss budget, feed-thread priority,
  lock-free sharing - [realtime.md](realtime.md).
- Blocking or unbudgeted CPU on an async worker, a `buffer_unordered` throttled
  by a heavy body, `#[instrument]` on a per-poll function -
  [async.md](async.md).
- `get_unchecked` on network-fed decode, fixed-size DSP kernels, size-hint-losing
  adapters in a sample loop - [dsp-layout.md](dsp-layout.md).

## Backlog

These detector ideas are recorded in red-flags but not yet ported to the
enforced ast-grep rules: `audit.alloc-in-loop`, `audit.push-in-loop`,
`audit.modulo-index`, `audit.float-sum-hot`, `audit.tracing-in-loop-hot`,
`audit.default-hashmap-hot`. A ban on the `*_fast` float intrinsics and a
`get_unchecked` census are both trivial ast-grep candidates.
