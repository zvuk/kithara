# `kithara-storage` — storage primitives for Kithara

`kithara-storage` provides the fundamental storage abstractions used throughout Kithara:
- **`StreamingResource`**: random-access storage for large media resources (segments, progressive downloads)
- **`AtomicResource`**: atomic whole-file operations for small metadata (playlists, keys, indexes)

This crate is intentionally minimal — it defines only the storage contracts and basic implementations.
Higher-level concerns (asset trees, eviction, leases, network orchestration) belong to other crates.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core abstractions
- `trait Resource` — Base contract for all storage resources
- `trait StreamingResourceExt` — Extension for random-access operations
- `trait AtomicResourceExt` — Marker trait for atomic resources

### Concrete implementations
- `struct StreamingResource` — Random-access storage with range waiting
- `struct AtomicResource` — Atomic whole-file storage

### Configuration and errors
- `struct DiskOptions` — Configuration for disk-backed resources
- `struct AtomicOptions` — Configuration for atomic resources
- `enum StorageError` — Error type for storage operations
- `type StorageResult<T> = Result<T, StorageError>` — Result alias

### Utilities
- `enum WaitOutcome` — Result of waiting for a range (Ready, Pending, Failed)

## Resource kinds and use cases

### `StreamingResource` — for large, progressive resources

Use when:
- Resource size is large (megabytes or more)
- Data arrives progressively (network downloads)
- Random access is needed (seek during playback)
- Consumers need to wait for specific ranges

Key operations:
- `write_at(offset, data)` — Write bytes at specific offset
- `read_at(offset, len)` — Read bytes from specific offset
- `wait_range(range)` — Wait for a range to become available
- `status()` — Get current resource status (Empty, Partial, Complete)

Typical usage in Kithara:
- Media segments in HLS/MP3 progressive downloads
- Large single-file assets

### `AtomicResource` — for small, atomic resources

Use when:
- Resource size is small (kilobytes or less)
- Whole-object semantics are required
- Crash-safe updates are needed
- No partial reads/writes are needed

Key operations:
- `write(data)` — Atomically replace entire resource
- `read()` — Read entire resource
- `commit()` — Finalize resource (mark as complete)
- `fail(error)` — Mark resource as failed

Typical usage in Kithara:
- Playlist files (.m3u8)
- Encryption keys
- Metadata indexes (pin tables, LRU indexes)
- Configuration files

## Core invariants

### For `StreamingResource`:
1. **No false EOF**: `read_at` returns `Ok(0)` only when the resource is complete and all data has been read
2. **Range waiting**: `wait_range` allows consumers to block until specific byte ranges are available
3. **Random access**: Supports reads/writes at arbitrary offsets
4. **Progressive filling**: Resource can be partially filled while being read

### For `AtomicResource`:
1. **Atomic updates**: `write` uses temp file → rename pattern for crash safety
2. **Whole-object**: Always reads/writes entire resource
3. **Lifecycle**: Resources have explicit `commit()` or `fail()` states

### For both resource types:
1. **Cancellation**: All async operations support cancellation via `CancellationToken`
2. **Error propagation**: Errors are typed and preserve context
3. **Thread safety**: All operations are `Send + Sync`

## Implementation details

### Disk-backed storage

Both resource types have disk-backed implementations:

- `StreamingResource` uses `random-access-disk` for efficient random access
- `AtomicResource` uses temporary files and atomic renames for crash safety

### Range waiting mechanism

`StreamingResource::wait_range` is essential for streaming playback:

```rust
// Consumer waits for specific range
match resource.wait_range(1024..2048, cancel).await {
    Ok(WaitOutcome::Ready) => {
        // Range is now available for reading
        let data = resource.read_at(1024, 1024).await?;
    }
    Ok(WaitOutcome::Pending) => {
        // Resource is still being filled, wait more
    }
    Ok(WaitOutcome::Failed) => {
        // Resource failed (e.g., download error)
    }
    Err(e) => {
        // Cancelled or other error
    }
}
```

This allows consumers to efficiently wait for data without busy polling.

## Integration with other Kithara crates

### `kithara-assets`
- Uses both `StreamingResource` and `AtomicResource` as building blocks
- `Assets` trait returns these resource types
- Implements asset-level semantics on top of storage primitives

### `kithara-stream`
- `Writer` writes to `StreamingResource` using `write_at`
- `Reader` reads from `StreamingResource` using `read_at` and `wait_range`
- Coordinates producer/consumer with storage as the buffer

### `kithara-hls` / `kithara-file`
- Use resources through `kithara-assets` abstraction
- Determine which resource type is appropriate for each file

## Error handling

`StorageError` covers all possible storage failures:

- `Io` — Underlying I/O error
- `Cancelled` — Operation was cancelled
- `InvalidRange` — Requested range is invalid
- `ResourceFailed` — Resource is in failed state
- `ResourceClosed` — Resource was closed
- `NotSupported` — Operation not supported by this resource type

## Configuration

### `DiskOptions`
- `path: PathBuf` — Filesystem path for the resource
- `create_dir: bool` — Whether to create parent directories

### `AtomicOptions`
- `path: PathBuf` — Filesystem path for the resource
- `temp_suffix: String` — Suffix for temporary files (default: `.tmp`)

## Example usage

### Creating and using a `StreamingResource`
```rust
use kithara_storage::{StreamingResource, DiskOptions};
use bytes::Bytes;
use tokio_util::sync::CancellationToken;

let options = DiskOptions {
    path: "/path/to/resource".into(),
    create_dir: true,
};

let resource = StreamingResource::open(options).await?;
let cancel = CancellationToken::new();

// Write data progressively
resource.write_at(0, Bytes::from("hello"), cancel.clone()).await?;
resource.write_at(5, Bytes::from("world"), cancel.clone()).await?;

// Read data back
let data = resource.read_at(0, 10, cancel.clone()).await?;

// Wait for a specific range
resource.wait_range(0..10, cancel.clone()).await?;
```

### Creating and using an `AtomicResource`
```rust
use kithara_storage::{AtomicResource, AtomicOptions};
use bytes::Bytes;
use tokio_util::sync::CancellationToken;

let options = AtomicOptions {
    path: "/path/to/metadata.json".into(),
    temp_suffix: ".tmp".to_string(),
};

let resource = AtomicResource::open(options).await?;
let cancel = CancellationToken::new();

// Write atomically
resource.write(Bytes::from("{\"key\": \"value\"}"), cancel.clone()).await?;
resource.commit().await?;

// Read back
let data = resource.read(cancel.clone()).await?;
```

## Design philosophy

1. **Minimal core**: This crate does only storage, nothing else
2. **Clear contracts**: Traits define exactly what each resource type can do
3. **Composition over inheritance**: Resources are designed to be composed by higher layers
4. **Async-first**: All operations are async and support cancellation
5. **Crash safety**: Atomic operations ensure consistency across failures