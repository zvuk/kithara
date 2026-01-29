<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-storage

Storage primitives for kithara. Provides two resource types: `StreamingResource` for random-access media (segments, progressive downloads) with `write_at`/`read_at`/`wait_range`, and `AtomicResource` for small metadata blobs (playlists, keys) with crash-safe atomic replace.

## Usage

```rust
use kithara_storage::{StreamingResource, DiskOptions};

let resource = StreamingResource::new(path, DiskOptions::default()).await?;
resource.write_at(0, &data).await?;
let outcome = resource.wait_range(0..1024, cancel.clone()).await?;
```

## Integration

Foundation layer for `kithara-assets`. Higher-level concerns (trees of resources, eviction, leases) are handled by `kithara-assets`.
