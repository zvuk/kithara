<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-hls.svg)](https://crates.io/crates/kithara-hls)
[![docs.rs](https://docs.rs/kithara-hls/badge.svg)](https://docs.rs/kithara-hls)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-hls

HLS (HTTP Live Streaming) VOD orchestration: playlist parsing, segment fetching, adaptive-bitrate decisions, cross-codec variant switching, AES-128-CBC decryption, and persistent caching. Implements `kithara_stream::StreamType` for use with `Stream<Hls>`.

## Usage

```rust
use kithara_stream::Stream;
use kithara_hls::{Hls, HlsConfig};

let config = HlsConfig::new(master_playlist_url);
let stream = Stream::<Hls>::new(config).await?;
// `stream` implements Read + Seek; pass it into kithara-decode / kithara-audio.
```

`HlsConfig` is a [`bon`](https://crates.io/crates/bon) builder. Use `HlsConfig::new(url)` for the URL-only shortcut, or `HlsConfig::for_url(url)` to start a chained builder for non-default settings (`look_ahead_bytes`, key options, downloader, asset store, cancel token, event bus).

## Key Public Items

- `Hls` — zero-sized `StreamType` marker for HLS streams.
- `HlsConfig` / `KeyOptions` — bon-builder stream configuration and DRM key-resolution options.
- `HlsSource` — the `Source` implementation that `Stream<Hls>` wraps.
- `KeyStore`, `PlaylistCache` — AES-128 key coordination and parsed-playlist cache.
- `parse_master_playlist`, `parse_media_playlist` — standalone playlist parsers.
- `HlsError` / `HlsResult` — crate error type and result alias.

`HlsCoord` and `HlsPeer` are internal orchestration types. Re-exports cover the
ABR mode plus DRM key-processor registry types used to configure encrypted HLS.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
