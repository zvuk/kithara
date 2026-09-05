<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-hls.svg)](https://crates.io/crates/kithara-hls)
[![docs.rs](https://docs.rs/kithara-hls/badge.svg)](https://docs.rs/kithara-hls)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-hls

HLS (HTTP Live Streaming) VOD orchestration: playlist parsing, segment fetching, adaptive-bitrate decisions, cross-codec variant switching, AES-128-CBC decryption, and persistent caching. Implements `kithara_stream::StreamType` for use with `Stream<Hls<S>>`, backed by an application-owned `PoolRegion<S>`.

## Usage

```rust
use kithara_assets::AssetStore;
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_hls::{Hls, HlsConfig};
use kithara_stream::Stream;

pool_schema! {
    AppPools {
        bytes: u8,
    }
}

let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(PoolConfig::builder().max_buffers(128).build())
    .build()?;
let store = AssetStore::builder(pools.clone()).build();
let config = HlsConfig::for_url(master_playlist_url)
    .store(store)
    .pools(pools)
    .build();
let stream = Stream::<Hls<AppPools>>::new(config).await?;
// `stream` implements Read + Seek; pass it into kithara-decode / kithara-audio.
```

`HlsConfig<S>` is a [`bon`](https://crates.io/crates/bon) builder. Start with `HlsConfig::for_url(url)`, set the required shared asset store with `.store(store)` and its matching region with `.pools(pools)`, then call `.build()`. Both values use the same schema `S`; cloned regions retain one shared hard budget. The same chain accepts key options, downloader, cache discriminator, cancel token, and event bus, plus the streaming knobs that do not depend on `S` — `net_options`, `size_probe_method`, `download_batch_size`, `acquire_attempt_budget`, the ephemeral-cache bounds, `event_channel_capacity`, and `look_ahead_bytes` — as setters of their own.

## Key Types

- `Hls<S>` - zero-sized `StreamType` marker for HLS streams using schema `S`.
- `HlsConfig<S>` / `KeyOptions` - bon-builder stream configuration and DRM key-resolution options.
- `HlsSource<S>` - the `Source` implementation that `Stream<Hls<S>>` wraps.
- `KeyStore`, `PlaylistCache` — AES-128 key coordination and parsed-playlist cache.
- `parse_master_playlist`, `parse_media_playlist` — standalone playlist parsers.
- `HlsError` / `HlsResult` — crate error type and result alias.

`HlsCoord` and `HlsPeer` are internal orchestration types. Re-exports cover the
ABR mode plus DRM key-processor registry types used to configure encrypted HLS.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
