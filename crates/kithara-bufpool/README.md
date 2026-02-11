<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-bufpool.svg)](https://crates.io/crates/kithara-bufpool)
[![Downloads](https://img.shields.io/crates/d/kithara-bufpool.svg)](https://crates.io/crates/kithara-bufpool)
[![docs.rs](https://docs.rs/kithara-bufpool/badge.svg)](https://docs.rs/kithara-bufpool)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-bufpool

Generic sharded buffer pool for zero-allocation hot paths. Provides thread-safe, RAII-guarded buffers that automatically return to the pool on drop. Supports any type implementing the `Reuse` trait (`Vec<u8>`, `Vec<f32>`, etc.).

## Usage

```rust
use kithara_bufpool::{SharedPool, BytePool, PcmPool, byte_pool, pcm_pool};

// Shared pool (Arc-wrapped, sharded by thread)
let pool = SharedPool::<32, Vec<u8>>::new(1024, 128 * 1024);
let mut buf = pool.get_with(|b| b.resize(4096, 0));
buf[0] = 42;
// buf returns to pool on drop

// Global singletons
let bytes = byte_pool().get_with(|b| b.resize(1024, 0));
let pcm = pcm_pool().get_with(|b| b.clear());
```

## Type aliases

| Alias | Definition | Global accessor |
|-------|-----------|----------------|
| `BytePool` | `SharedPool<32, Vec<u8>>` | `byte_pool()` |
| `PcmPool` | `SharedPool<32, Vec<f32>>` | `pcm_pool()` |

## Allocation Flow

1. **Get:** Lock home shard (determined by thread ID hash) and pop a buffer. If empty, try other shards (work-stealing). If all empty, allocate a new buffer via `T::default()`. Apply the initialization closure.
2. **Return (drop):** Call `value.reuse(trim_capacity)` to clear and optionally shrink. If the shard is not full and reuse returns `true`, push back. Otherwise, drop silently.

## Global Pools

| Pool | Type | Max Buffers | Trim Capacity |
|------|------|-------------|---------------|
| `byte_pool()` | `SharedPool<32, Vec<u8>>` | 1024 | 64 KB |
| `pcm_pool()` | `SharedPool<32, Vec<f32>>` | 64 | 200K |

Additional global pools can be defined with the `global_pool!` macro.

## Integration

Used across the entire kithara workspace to eliminate allocations on hot paths (segment reads, PCM decode/resample, network I/O). Global pools are lazy-initialized via `OnceLock`.
