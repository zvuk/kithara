# `kithara-bufpool` — Generic sharded buffer pool

`kithara-bufpool` provides a thread-safe, generic buffer pool with sharding to eliminate allocations in hot paths.

## Features

- **Generic over T**: Works with `Vec<u8>`, `Vec<f32>`, `Vec<f64>`, or any type implementing `Reuse`
- **Sharded design**: Reduces lock contention via multiple independent shards
- **RAII safety**: Buffers automatically return to pool on drop
- **Zero-copy**: Reuses allocated memory instead of repeated alloc/dealloc
- **Thread-safe**: Can be safely shared across threads

## Public contract (normative)

### Core types

- `trait Reuse` — Trait for types that can be pooled and reused
- `struct Pool<const SHARDS: usize, T>` — Main sharded pool (borrowed access)
- `struct Pooled<'a, const SHARDS: usize, T>` — RAII wrapper for pooled buffer (borrowed)
- `struct PooledSlice<'a, const SHARDS: usize, T>` — Pooled Vec with tracked length (borrowed)
- `struct SharedPool<const SHARDS: usize, T>` — Arc-wrapped pool for sharing
- `struct PooledOwned<const SHARDS: usize, T>` — RAII wrapper for pooled buffer (owned Arc)
- `struct PooledSliceOwned<const SHARDS: usize, T>` — Pooled Vec with tracked length (owned Arc)

### Provided implementations

- `impl Reuse for Vec<T>` — Generic Vec reuse (clears and shrinks to trim size)

## Architecture

```
Pool<32, Vec<f32>>
├─ Shard 0: Mutex<[Vec<f32>, ...]>
├─ Shard 1: Mutex<[Vec<f32>, ...]>
├─ Shard 2: Mutex<[Vec<f32>, ...]>
...
└─ Shard 31: Mutex<[Vec<f32>, ...]>

Thread assignment via hash(thread_id) % SHARDS
```

### Sharding strategy

- Thread ID hashed to shard index for even distribution
- Each shard has independent lock (parking_lot::Mutex)
- If primary shard empty, tries other shards before allocating
- Reduces contention in multi-threaded scenarios

## Usage patterns

### Static global pool (recommended)

Use the `global_pool!` macro for convenient global pool declaration:

```rust
use kithara_bufpool::global_pool;

// Declare a global pool for Vec<f32>
// Params: fn_name, static_name, shards, type, max_buffers, trim_capacity
global_pool!(f32_pool, F32_POOL, 32, Vec<f32>, 1024, 32_768);

// Usage - call the generated function
let mut buf = f32_pool().get();
buf.resize(4096, 0.0);
// ... use buf ...
// Automatically returned to pool on drop
```

The macro expands to:
```rust
static F32_POOL: OnceLock<&'static Pool<32, Vec<f32>>> = OnceLock::new();

fn f32_pool() -> &'static Pool<32, Vec<f32>> {
    F32_POOL.get_or_init(|| {
        let pool = Pool::<32, Vec<f32>>::new(1024, 32_768);
        Box::leak(Box::new(pool))
    })
}
```

### PooledSlice for length tracking

```rust
use kithara_bufpool::{Pool, PooledSlice, SharedPool, PooledSliceOwned};

// With borrowed Pool (lifetime 'a)
let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
let mut buf = pool.get_with(|b| b.resize(4096, 0));

// Read data (e.g., from network or file)
let bytes_read = read_data_into(&mut buf)?; // Returns actual bytes read

// Wrap in PooledSlice to track actual length
let slice = PooledSlice::new(buf, bytes_read);
assert_eq!(slice.as_slice().len(), bytes_read);

// Use as &[u8] via AsRef
let data: &[u8] = slice.as_ref();

// With SharedPool (owned Arc, 'static-compatible)
let shared_pool = SharedPool::<32, Vec<u8>>::new(1024, 128 * 1024);
let mut buf = shared_pool.get_with(|b| b.resize(4096, 0));
let bytes_read = read_data_into(&mut buf)?;

// PooledSliceOwned has no lifetime parameter
let slice = PooledSliceOwned::new(buf, bytes_read);
// Can be used in 'static contexts like async_stream::stream!
```

### Initialization callback

```rust
let buf = pool.get_with(|b| b.resize(1024, 0.0));
assert_eq!(buf.len(), 1024);
```

### Shared pool (Arc-wrapped)

```rust
use kithara_bufpool::SharedPool;

let pool = SharedPool::<32, Vec<u8>>::new(1024, 128 * 1024);
let pool_clone = pool.clone(); // Arc::clone

// Returns PooledOwned (no lifetime, 'static-compatible)
let buf = pool.get_with(|b| b.resize(1024, 0));

// Can be shared across threads and used in async contexts
std::thread::spawn(move || {
    let buf = pool_clone.get();
    // ...
});
```

**Key difference:** `SharedPool::get()` returns `PooledOwned` (owns `Arc<Pool>`), not `Pooled<'a>`. This makes it `'static`-compatible and usable in contexts like `async_stream::stream!` that require `'static` lifetimes.

## Parameters

### SHARDS (const generic)

Number of independent shards in the pool.

- Typical values: 16, 32, 64
- Trade-off: More shards = less contention, but more memory overhead
- Rule of thumb: Use 2× or 4× number of cores

### max_buffers

Maximum total buffers across all shards.

- Distributed evenly: `buffers_per_shard = max_buffers / SHARDS`
- When shard full, oldest buffers dropped (not returned to pool)

### trim_capacity

Shrink buffers to this capacity when returning to pool.

- Prevents unbounded memory growth
- Example: `128 * 1024` = trim to 128KB
- Set to expected max size in your workload

## Performance considerations

### When to use

✅ **Good fit**:
- Hot paths with frequent allocations (>10/sec)
- Buffers transferred between components (ownership moves)
- Predictable buffer sizes
- Multi-threaded workloads

❌ **Not needed**:
- Buffers reused in single loop (just use `Vec::clear()`)
- Cold paths (infrequent allocations)
- Buffers never leave scope

### Overhead

- **Pool lookup**: ~10-20ns (hash + mutex lock)
- **Allocation saved**: ~100-1000ns (depends on allocator and size)
- **Net benefit**: 5-50x faster than allocate/deallocate

## Integration with Kithara crates

### kithara-stream

```rust
static BYTE_POOL: Pool<32, Vec<u8>>;  // For network I/O buffers
```

### kithara-decode

```rust
static F32_POOL: Pool<32, Vec<f32>>;  // For audio PCM buffers
```

## Design philosophy

1. **Generic first**: Don't hardcode to `Vec<u8>`, support any `T`
2. **Sharded for speed**: Minimize lock contention
3. **RAII safety**: No manual `return_to_pool()` calls
4. **Bounded growth**: Trim capacity to prevent memory leaks
5. **Zero unsafe**: All safe Rust

## Comparison with buffer-pool

| Feature | buffer-pool | kithara-bufpool |
|---------|-------------|-----------------|
| Generic over T | ❌ (only Vec<u8>) | ✅ (any T: Reuse) |
| Sharding | ✅ | ✅ |
| RAII | ✅ | ✅ |
| Vec<f32> support | ❌ | ✅ |
| Orphan rule issues | ✅ (can't impl Reuse) | ❌ (our trait) |

## Custom Reuse implementations

You can implement `Reuse` for custom types:

```rust
use kithara_bufpool::Reuse;

struct MyBuffer {
    data: Vec<u8>,
    metadata: usize,
}

impl Reuse for MyBuffer {
    fn reuse(&mut self, trim: usize) -> bool {
        self.data.clear();
        self.data.shrink_to(trim);
        self.metadata = 0;
        self.data.capacity() > 0
    }
}

// Now can use Pool<32, MyBuffer>
```
