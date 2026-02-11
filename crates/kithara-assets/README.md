<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![Downloads](https://img.shields.io/crates/d/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![docs.rs](https://docs.rs/kithara-assets/badge.svg)](https://docs.rs/kithara-assets)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-assets

Assets store (disk or in-memory) with lease/pin semantics and LRU eviction. An *asset* is a logical unit addressed by `asset_root` containing multiple *resources* addressed by `rel_path`. All resources use the unified `StorageResource` from `kithara-storage`. `AssetStore` provides file-backed (mmap) storage; `MemAssetStore` / `MemStore` provide ephemeral in-memory storage.

## Usage

```rust
use kithara_assets::{AssetStoreBuilder, ResourceKey};

let store = AssetStoreBuilder::new()
    .root_dir(cache_dir)
    .asset_root("asset123")
    .cancel(cancel.clone())
    .build();
let key = ResourceKey::new("asset123", "segments/001.m4s");
let resource = store.open_resource(&key)?;
```

## Decorator Chain

Requests flow through four layers (outermost to innermost):

| Layer | Responsibility |
|-------|---------------|
| `CachedAssets` | In-memory LRU cache (default 5 entries); prevents duplicate mmap opens |
| `LeaseAssets` | RAII-based pinning; `LeaseGuard` unpins on drop; prevents eviction of in-use assets |
| `ProcessingAssets` | Optional chunk-based transformation on `commit()` (e.g., AES-128-CBC decryption) |
| `EvictAssets` | LRU eviction by asset count and/or byte size; pinned assets excluded |
| `DiskAssetStore` | Base disk I/O; maps `ResourceKey` to filesystem paths |

```mermaid
sequenceDiagram
    participant Caller
    participant Cache as CachedAssets
    participant Lease as LeaseAssets
    participant Proc as ProcessingAssets
    participant Evict as EvictAssets
    participant Disk as DiskAssetStore
    participant FS as Filesystem

    Caller->>Cache: open_resource_with_ctx(key, ctx)
    Cache->>Cache: check LRU cache
    alt Cache hit
        Cache-->>Caller: cached resource
    else Cache miss
        Cache->>Lease: open_resource_with_ctx(key, ctx)
        Lease->>Lease: pin(asset_root)
        Lease->>Lease: persist _index/pins.bin
        Lease->>Proc: open_resource_with_ctx(key, ctx)
        Proc->>Evict: open_resource_with_ctx(key, ctx)
        Evict->>Evict: first access? check LRU limits
        alt Over limits
            Evict->>Evict: evict oldest unpinned assets
        end
        Evict->>Disk: open_resource_with_ctx(key, ctx)
        Disk->>FS: MmapResource::open(path, mode)
        FS-->>Disk: StorageResource::Mmap
        Disk-->>Evict: StorageResource
        Evict-->>Proc: StorageResource
        Proc->>Proc: wrap in ProcessedResource(res, ctx)
        Proc-->>Lease: ProcessedResource
        Lease->>Lease: wrap in LeaseResource(res, guard)
        Lease-->>Cache: LeaseResource
        Cache->>Cache: insert into LRU cache
        Cache-->>Caller: LeaseResource
    end

    Note over Caller: On commit():
    Caller->>Proc: commit(final_len)
    alt ctx is Some (encrypted)
        Proc->>Proc: read 64KB chunks from disk
        Proc->>Proc: aes128_cbc_process_chunk()
        Proc->>Proc: write decrypted back to disk
    end
    Proc->>Disk: commit(actual_len)

    Note over Caller: On drop(LeaseResource):
    Caller->>Lease: drop LeaseGuard
    Lease->>Lease: unpin(asset_root)
    Lease->>Lease: persist _index/pins.bin
```

## Index Persistence

Three index types are persisted under `_index/` for crash recovery:

| Index | File | Purpose |
|-------|------|---------|
| Pins | `_index/pins.bin` | Persists pinned asset roots |
| LRU | `_index/lru.bin` | Monotonic clock + byte accounting for eviction |
| Coverage | `_index/cov.bin` | Per-segment byte-range coverage for partial downloads |

All indices use bincode serialization with `Atomic<R>` for crash-safe writes.

## Integration

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides `AssetStore` type alias composing decorators: `LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<DiskAssetStore>>>>`. Also provides `MemAssetStore` / `MemStore` for ephemeral in-memory usage.
