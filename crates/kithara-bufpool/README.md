<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-bufpool

Generic sharded buffer pool for zero-allocation hot paths. Provides thread-safe, RAII-guarded buffers that automatically return to the pool on drop. Supports any type implementing the `Reuse` trait (`Vec<u8>`, `Vec<f32>`, etc.).

## Usage

```rust
use kithara_bufpool::{Pool, SharedPool};

// Create a shared pool (Arc-wrapped)
let pool = SharedPool::<32, Vec<u8>>::new(1024, 128 * 1024);

// Get a buffer, use it, auto-return on drop
let mut buf = pool.get_with(|b| b.resize(4096, 0));
buf[0] = 42;
```

## Integration

Used across the entire kithara workspace to eliminate allocations on hot paths (segment reads, chunk processing, network I/O, PCM output). Accessed via `kithara-assets` re-exports (`BytePool`, `byte_pool`).
