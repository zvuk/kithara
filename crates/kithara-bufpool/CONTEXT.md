# kithara-bufpool — Context

Detailed contracts and invariants for the kithara-bufpool crate; the README is the overview.

## Allocation Flow

Pool `get`/`put` are **lock-free**: each shard is a bounded `crossbeam_queue::ArrayQueue`, so both producer and consumer recycle buffers without taking a lock — safe to call on the real-time produce/consume cores.

1. **Get:** `pop` from the home shard (`thread_id % SHARDS`). If empty, probe up to `MAX_PROBE` neighbour shards (currently 4) for work-stealing. If those probes miss, allocate a new buffer via `T::default()`; other unprobed shards may still hold buffers.
2. **Return (drop):** call `value.reuse(trim_capacity)` to clear and optionally shrink, then `push` onto the home shard. If `reuse()` rejects the buffer or the queue is full, the buffer is dropped and its bytes released from the budget.

Each shard's queue capacity is fixed at construction (`max_buffers / SHARDS`, clamped to a sane upper bound for count-unbounded pools such as `BytePool`). The byte budget — not the slot count — is the real memory cap for those pools.

## Region and the Shared Budget

`Region` is the canonical owner of one byte budget shared by a `BytePool` and a `PcmPool`. Composition roots (app `main`, FFI `NativeInner`, a standalone `Queue` that builds its own player) construct one `Region` and pass region-derived pools down through configs; library code never calls `BytePool::default()` / `PcmPool::default()` outside tests. Standalone pools built via `new`/`with_byte_budget` own a private budget and behave as before.

The budget counts **admitted pool capacity in bytes**, not RSS: allocator metadata, rounding, transient copies during growth, and plain `Vec`s outside the pool (e.g. time-stretch scratch) are not covered.

### Travelling charge

A buffer's byte charge is acquired at first growth (`ensure_len`) and released only when a return is rejected (`put` into a full shard or `reuse()` refusal). The charge **travels with the buffer**: `PooledOwned::into_inner()` does not release it, and `attach()` does not charge — so `into_inner → attach` round-trips (the time-stretch planar scratch) stay balanced. Two consequences:

- `attach` is only for values whose capacity this pool already accounts for; importing genuinely external memory needs a charging API, which is deliberately absent until a production consumer exists.
- Extracting a buffer via `into_inner` and dropping it without `recycle` leaks accounting (the bytes stay counted). All current call sites recycle in `Drop`.

### Controlled growth

`ensure_len` is transactional: it reserves the budget delta **before** allocating (`try_reserve_exact`), reconciles the actual capacity against the reservation, and rolls back fully on any failure — a failed call leaves length, capacity, and budget untouched. Growth is amortized (doubling, falling back to the exact request when the budget cannot afford the double) so incremental `ensure_len` loops stay O(n).

Raw `Vec` growth through `DerefMut` (`resize`, `extend_from_slice`, …) bypasses enforcement; such post-init growth past the cap is counted in `PoolStats::budget_overshoots` / `RegionStats::budget_overshoots` rather than blocked. Migrating the remaining raw-growth call sites and adding a static check is a documented follow-up together with the `PcmChunk::Default`/`Clone` ownership contract (both still allocate from the global PCM pool).

## Integration

Used across the workspace to eliminate allocations on hot paths (segment reads, PCM decode and resample, network I/O). Pools are wired through `Config` structs (`AudioConfig::byte_pool` / `pcm_pool`, `FileConfig` / `HlsConfig`, `ResamplerSettings::pcm_pool`) so each surface is responsible for choosing its own pool sizing.

## Lower-level re-exports

The lower-level `SharedPool`, `Pool`, `Pooled`, `PooledOwned`, `Reuse`, and `PoolStats` items are re-exported as `#[doc(hidden)]` for internal use by other workspace crates.

## Feature Flags

- `perf` — enables `hotpath` instrumentation on pool hot paths.
