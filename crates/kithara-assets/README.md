<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![docs.rs](https://docs.rs/kithara-assets/badge.svg)](https://docs.rs/kithara-assets)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-assets

Assets store (disk or in-memory) with lease/pin semantics and LRU eviction. An *asset* is a logical source containing one or more semantic *resources*. An `AssetLayout` maps those values to `<asset_root>/<resource_path>`; callers cannot inject a preformed relative cache path. One cheap, `Arc`-backed `AssetStore<S>` handle can be cloned into every file, HLS, playback, and analysis consumer while retaining one backend, layout registry, index set, eviction policy, and application-owned `PoolRegion<S>`.

## Usage

```rust
use kithara_assets::{
    AssetResource, AssetSource, AssetStore, StorageBackend,
};
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};

pool_schema! {
    AppPools {
        bytes: u8,
    }
}

struct Protocol;

let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(PoolConfig::builder().max_buffers(128).build())
    .build()?;
let store = AssetStore::builder(pools.clone())
    .backend(StorageBackend::Disk { root: cache_dir })
    .cancel(cancel.clone())
    .build();
let source = AssetSource::Remote {
    url,
    discriminator: None,
};
let scope = store.scope::<Protocol>(&source)?;
let key = scope.key(&AssetResource::Source {
    extension: "mp3".to_string(),
})?;
let resource = store.acquire_resource(&key, None)?;
```

## Key Types

- `AssetStore<S>` - the unified shared handle over `Disk` or `Memory` storage.
- `AssetStore::builder(pools)` - requires the application-owned `PoolRegion<S>` and configures the store and its immutable layout registry.
- `AssetLayout` / `AssetLayoutRegistry` — own cache-root and resource-path policy, with optional overrides selected by protocol marker type.
- `AssetSource` / `AssetResource` — semantic input to the selected layout.
- `AssetScope` / `ResourceKey` — validated output used by cache operations.
- `AssetStore::attach_pending_resource(&key, read_pos, look_ahead)` - joins or creates the canonical pending-resource acquisition used by protocol crates.
- `ResourceAcquisition` — the Pending/Ready `AcquisitionResult<AssetWriter, AssetReader>` surfaced by the facade.

### Public contract

The public storage contract is `AssetStore<S>` plus the source/resource/layout/key types used to mint validated keys. Decorator and backend implementation types are not configuration alternatives. Define the closed pool schema at the application boundary, build its `PoolRegion<S>` once, construct the store with `AssetStore::builder(pools)`, and pass cheap clones of the store and region to consumers that share the same hard budget.

## Features

- `client-reqwest` / `client-wreq` and `tls-rustls` / `tls-native` — forward network backend selection to storage/test-utils dependencies.
- `usdt` — USDT probes for tracing.
- `mock` — enables generated mocks for tests.

## Integration

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides a unified `AssetStore<S>` type (`Disk`/`Mem`) that internally composes decorators: `LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<...>>>>`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-assets) for detailed contracts, invariants, and internals.
