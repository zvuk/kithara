<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-hls

HLS (HTTP Live Streaming) VOD orchestration with adaptive bitrate, persistent caching, and encryption key management. Implements `StreamType` for use with `Stream<Hls>`, coordinating playlist parsing, segment fetching, ABR decisions, and disk cache.

## Usage

```rust
use kithara_stream::Stream;
use kithara_hls::{Hls, HlsConfig};

let config = HlsConfig::new(url);
let stream = Stream::<Hls>::new(config).await?;
```

## Integration

Depends on `kithara-net` for HTTP, `kithara-assets` for caching, and `kithara-abr` for ABR algorithm. Composes with `kithara-decode` as `Decoder<Stream<Hls>>`. Emits `HlsEvent` via broadcast channel for monitoring.
