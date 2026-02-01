<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
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

## Integration

Used across the entire kithara workspace to eliminate allocations on hot paths (segment reads, PCM decode/resample, network I/O). Global pools are lazy-initialized via `OnceLock`.
