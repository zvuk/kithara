<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-bufpool.svg)](https://crates.io/crates/kithara-bufpool)
[![docs.rs](https://docs.rs/kithara-bufpool/badge.svg)](https://docs.rs/kithara-bufpool)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-bufpool

Generic sharded buffer pool for zero-allocation hot paths. Provides thread-safe, RAII-guarded buffers that automatically return to the pool on drop. Supports any type implementing the `Reuse` trait (`Vec<u8>`, `Vec<f32>`, etc.).

## Usage

```rust
use kithara_bufpool::{BytePool, PcmPool};

// Use the default singleton — typically constructed once at the top of the
// app and injected through your config structs. Library code should read
// the pool from injected config rather than calling `default()` itself.
let pool = PcmPool::default();
let mut buf = pool.get();
buf.resize(1024, 0.0);
// `buf` returns to the pool on drop.

let bytes = BytePool::default();
let mut chunk = bytes.get();
chunk.resize(4096, 0);
```

## Public Types

<table>
<tr><th>Type</th><th>Role</th></tr>
<tr><td><code>BytePool</code></td><td>Sharded pool of <code>Vec&lt;u8&gt;</code> for I/O and segment buffers</td></tr>
<tr><td><code>PcmPool</code></td><td>Sharded pool of <code>Vec&lt;f32&gt;</code> for decoded PCM frames</td></tr>
<tr><td><code>PcmBuf</code></td><td>RAII handle for a pooled PCM buffer</td></tr>
<tr><td><code>ByteBudget</code></td><td>Soft cap on outstanding bytes; returns <code>BudgetExhausted</code> when exceeded</td></tr>
<tr><td><code>BudgetExhausted</code></td><td>Error returned when a budgeted allocation would breach <code>ByteBudget</code></td></tr>
</table>

The lower-level `SharedPool`, `Pool`, `Pooled`, `PooledOwned`, `Reuse`, and `PoolStats` items are re-exported as `#[doc(hidden)]` for internal use by other workspace crates.

## Allocation Flow

Pool `get`/`put` are **lock-free**: each shard is a bounded [`crossbeam_queue::ArrayQueue`], so both producer and consumer recycle buffers without taking a lock — safe to call on the real-time produce/consume cores.

1. **Get:** `pop` from the home shard (determined by thread ID hash). If empty, `pop` from neighbour shards (work-stealing). If all empty, allocate a new buffer via `T::default()`.
2. **Return (drop):** call `value.reuse(trim_capacity)` to clear and optionally shrink, then `push` onto the home shard. If `reuse()` rejects the buffer or the queue is full, the buffer is dropped and its bytes released from the budget.

Each shard's queue capacity is fixed at construction (`max_buffers / SHARDS`, clamped to a sane upper bound for count-unbounded pools such as `BytePool`). The byte budget — not the slot count — is the real memory cap for those pools.

## Integration

Used across the workspace to eliminate allocations on hot paths (segment reads, PCM decode and resample, network I/O). Pools are wired through `Config` structs (`AudioConfig::byte_pool` / `pcm_pool`, `FileConfig` / `HlsConfig`, `ResamplerParams::pool`) so each surface is responsible for choosing its own pool sizing.
