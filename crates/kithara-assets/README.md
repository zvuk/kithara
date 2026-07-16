<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![docs.rs](https://docs.rs/kithara-assets/badge.svg)](https://docs.rs/kithara-assets)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-assets

Assets store (disk or in-memory) with lease/pin semantics and LRU eviction. An *asset* is a logical source containing one or more semantic *resources*. An `AssetLayout` maps those values to `<asset_root>/<resource_path>`; callers cannot inject a preformed relative cache path. One cheap, `Arc`-backed `AssetStore` handle can be cloned into every file, HLS, playback, and analysis consumer while retaining one backend, layout registry, index set, and eviction policy.

## Role

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides a unified `AssetStore` type (`Disk`/`Mem`) that internally composes decorators: `LeaseAssets<CachedAssets<ProcessingAssets<EvictAssets<...>>>>`.

## Key types & entry points

- `AssetStore` — the unified shared handle over `Disk` or `Memory` storage.
- `AssetStoreBuilder` — constructs the store and its immutable layout registry.
- `AssetLayout` / `AssetLayoutRegistry` — own cache-root and resource-path policy, with optional overrides selected by protocol marker type.
- `AssetSource` / `AssetResource` — semantic input to the selected layout.
- `AssetScope` / `ResourceKey` — validated output used by cache operations.
- `attach_demand(&key, read_pos, look_ahead)` — registers a consumer with the single-producer demand index.
- `ResourceAcquisition` — the Pending/Ready `AcquisitionResult<AssetWriter, AssetReader>` surfaced by the facade.

## Usage

```rust
use kithara_assets::{
    AssetResource, AssetSource, AssetStoreBuilder, StorageBackend,
};

struct Protocol;

let store = AssetStoreBuilder::default()
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

## Features

- `client-reqwest` / `client-wreq` and `tls-rustls` / `tls-native` — forward network backend selection to storage/test-utils dependencies.
- `probe` — enables USDT probes for tracing.
- `mock` — enables generated mocks for tests.

## Public contract

The public storage contract is `AssetStore` plus the source/resource/layout/key types used to mint validated keys. Decorator and backend implementation types are not configuration alternatives. Construct one store with `AssetStoreBuilder` and pass cheap clones of that handle to consumers.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
