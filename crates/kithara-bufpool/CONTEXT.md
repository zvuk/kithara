# kithara-bufpool — Context

Detailed contracts and invariants for the kithara-bufpool crate; the README is the overview.

## Allocation Flow

Pool `get`/`put` are **lock-free**: each shard is a bounded `crossbeam_queue::ArrayQueue`, so both producer and consumer recycle buffers without taking a lock — safe to call on the real-time produce/consume cores.

1. **Get:** `pop` from the home shard (`thread_id % SHARDS`). If empty, probe up to `MAX_PROBE` neighbour shards (currently 4) for work-stealing. If those probes miss, allocate a new buffer via `T::default()`; other unprobed shards may still hold buffers.
2. **Return (drop):** call `value.reuse(trim_capacity)` to clear and optionally shrink, then `push` onto the home shard. If `reuse()` rejects the buffer or the queue is full, the buffer is dropped and its bytes released from the budget.

Each shard's queue capacity is fixed at construction (`max_buffers / SHARDS`, clamped to a sane upper bound for count-unbounded pools such as `BytePool`). The byte budget — not the slot count — is the real memory cap for those pools.

## Integration

Used across the workspace to eliminate allocations on hot paths (segment reads, PCM decode and resample, network I/O). Pools are wired through `Config` structs (`AudioConfig::byte_pool` / `pcm_pool`, `FileConfig` / `HlsConfig`, `ResamplerSettings::pcm_pool`) so each surface is responsible for choosing its own pool sizing.

## Lower-level re-exports

The lower-level `SharedPool`, `Pool`, `Pooled`, `PooledOwned`, `Reuse`, and `PoolStats` items are re-exported as `#[doc(hidden)]` for internal use by other workspace crates.

## Feature Flags

- `perf` — enables `hotpath` instrumentation on pool hot paths.
