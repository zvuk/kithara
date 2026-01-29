<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-assets

Persistent disk assets store with lease/pin semantics and LRU eviction. An *asset* is a logical unit addressed by `asset_root` containing multiple *resources* addressed by `rel_path`. Supports both streaming (segments) and atomic (playlists, keys) resources.

## Usage

```rust
use kithara_assets::{AssetStoreBuilder, ResourceKey};

let store = AssetStoreBuilder::new(cache_dir)
    .build(cancel.clone())
    .await?;
let key = ResourceKey::new("asset123", "segments/001.m4s");
let resource = store.streaming(&key).await?;
```

## Integration

Sits between `kithara-storage` (low-level I/O) and protocol crates (`kithara-file`, `kithara-hls`). Provides `AssetStore` type alias composing decorators: `LeaseAssets<EvictAssets<DiskAssetStore>>`.
