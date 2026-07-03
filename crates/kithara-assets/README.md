<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-assets.svg)](https://crates.io/crates/kithara-assets)
[![docs.rs](https://docs.rs/kithara-assets/badge.svg)](https://docs.rs/kithara-assets)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-assets

Assets store (disk or in-memory) with lease/pin semantics and LRU eviction. An *asset* is a logical unit addressed by `asset_root` containing multiple *resources* addressed by `rel_path`. A single `AssetStore` services every asset under its `root_dir`: `asset_root` is a per-request argument, so one shared store can back many URLs (e.g. app-wide playback + waveform). All resources use the unified `StorageResource` from `kithara-storage`. `AssetStore` is a unified wrapper with two backends: file-backed mmap (`Disk`) and ephemeral in-memory storage (`Mem`).

## Role

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides a unified `AssetStore` type (`Disk`/`Mem`) that internally composes decorators: `CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<...>>>>`.

## Key types & entry points

- `AssetStore` — the unified, explicit public contract (`Disk`/`Mem` backends).
- `AssetStoreBuilder` — constructs a store; propagates the shared cancellation token.
- `scope(asset_root)` — binds a scope to one `asset_root`; mints self-identifying keys and hosts per-resource ops (`acquire_resource`, `open_resource`, byte-availability queries).
- `attach_demand(&key, read_pos, look_ahead)` — registers a consumer with the single-producer demand index.
- `AcquisitionResult<Pending(ProcessedWriter), Ready(ProcessedReader)>` — the Pending/Ready typestate split that shapes the whole `Assets` contract.

## Usage

```rust
use kithara_assets::{AssetStoreBuilder, StorageBackend};

let store = AssetStoreBuilder::default()
    .backend(StorageBackend::Disk { root: cache_dir })
    .cancel(cancel.clone())
    .build();
// Bind a scope to one `asset_root`; it mints self-identifying keys.
let scope = store.scope("asset123");
let key = scope.key("segments/001.m4s");
// Per-resource ops live on the store; request identity is passed per call.
let resource = scope.store().acquire_resource(&key, None)?;
```

## Public contract

The explicit public contract is the unified `AssetStore` type. Everything else should be considered an implementation detail (even if currently `pub`), and constructors must propagate the shared cancellation token (use `AssetStoreBuilder`).

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
