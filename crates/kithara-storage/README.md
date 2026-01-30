<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-storage

Storage primitives for kithara. Provides a unified `StorageResource` backed by `mmap-io` with random-access `read_at`/`write_at`, blocking `wait_range`, and convenience `read_into`/`write_all`. `OpenMode` controls file access: `Auto` (default), `ReadWrite`, or `ReadOnly`.

## Usage

```rust
use kithara_storage::{StorageResource, StorageOptions, OpenMode};

let resource = StorageResource::open(StorageOptions {
    path: path.into(),
    initial_len: None,
    mode: OpenMode::Auto,
    cancel: cancel_token,
})?;
resource.write_at(0, &data)?;
let outcome = resource.wait_range(0..1024)?;
```

## Blocking coordination

```mermaid
sequenceDiagram
    participant Writer as Async writer (tokio task)
    participant SR as StorageResource
    participant CV as Condvar
    participant Reader as Sync reader (blocking)

    Writer->>SR: write_at(0, chunk_1)
    SR->>CV: notify_all()

    Reader->>SR: wait_range(0..4096)
    SR->>CV: wait (blocks)
    Note over CV,Reader: thread blocked until range available

    Writer->>SR: write_at(1024, chunk_2)
    SR->>CV: notify_all()

    Writer->>SR: write_at(2048, chunk_3)
    SR->>CV: notify_all()
    CV-->>Reader: wakeup, range satisfied
    SR-->>Reader: WaitOutcome::Satisfied
```

`wait_range` blocks the calling thread via `parking_lot::Condvar` until the requested byte range is fully written, or the resource reaches EOF/error/cancellation.

## Integration

Foundation layer for `kithara-assets`. Higher-level concerns (trees of resources, eviction, leases) are handled by `kithara-assets`.
