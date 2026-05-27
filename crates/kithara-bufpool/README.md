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

1. **Get:** lock the home shard (determined by thread ID hash) and pop a buffer. If empty, try other shards (work-stealing). If all empty, allocate a new buffer via `T::default()`.
2. **Return (drop):** call `value.reuse(trim_capacity)` to clear and optionally shrink. If the shard is not full and `reuse()` returns `true`, push back; otherwise drop silently.

## Integration

Used across the workspace to eliminate allocations on hot paths (segment reads, PCM decode and resample, network I/O). Pools are wired through `Config` structs (`AudioConfig::byte_pool` / `pcm_pool`, `FileConfig` / `HlsConfig`, `ResamplerParams::pool`) so each surface is responsible for choosing its own pool sizing.
