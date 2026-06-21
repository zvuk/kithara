<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara.svg)](https://crates.io/crates/kithara)
[![docs.rs](https://docs.rs/kithara/badge.svg)](https://docs.rs/kithara)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara

A streaming audio engine for Rust. Point it at a URL and it plays: `.m3u8`
streams adaptively over HLS, everything else downloads progressively. One
`Resource` type gives you a unified PCM read/seek interface. The player engine
underneath adds multi-deck mixing, crossfade, and parametric EQ for DJ and
pro-audio apps.

- **Auto-detecting** — HLS for `.m3u8`, progressive download otherwise.
- **Adaptive bitrate** for HLS, with segment caching and offline playback.
- **Gapless** decode across MP3, AAC (incl. HE-AAC), FLAC, ALAC, WAV, Opus, Vorbis.
- **Hardware or software decode** — Apple AudioToolbox and Android MediaCodec
  backends, or the cross-platform Symphonia software decoder.
- **DRM** — AES-128 decryption for protected HLS.

`kithara` is the facade crate: it aggregates the engine layers
(`audio`, `decode`, `events`, `platform`, `play`, `stream`, and the feature-gated
`file`/`hls`/`assets`/`net`/`storage` pipelines) behind one dependency and a
single `Resource` entry point.

## Usage

```rust
use kithara::prelude::*;

let config = ResourceConfig::new("https://example.com/song.mp3")?;
let mut resource = Resource::new(config).await?;
resource.preload().await?;

let mut buf = [0.0f32; 1024];
loop {
    match resource.read(&mut buf)? {
        ReadOutcome::Frames { count, .. } => play(&buf[..count.get()]),
        ReadOutcome::Pending { .. } => continue, // buffering / seeking
        ReadOutcome::Eof { .. } => break,
    }
}
```

`Resource` is a type-erased `Box<dyn PcmReader>`: the same `read()` / `seek()`
interface whether the source is HLS, a remote file, or a local path. Build it
from a `ResourceConfig`; `ReadOutcome` reports `Frames` / `Pending` / `Eof`.
The optional `EventBus` (`resource.event_bus()`) is an observability
side-channel and never sits in the audio path. For advanced control — multi-slot
engine, crossfade, EQ — reach into `kithara::play`. The `prelude` collects the
everyday types.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
