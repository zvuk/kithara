<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

# kithara

Facade crate providing a unified API for audio streaming, decoding, and playback. Auto-detects source type from URL (`.m3u8` = HLS, everything else = progressive file) and exposes a type-erased `Resource` with a simple `read()`/`seek()` interface.

## Usage

```rust
use kithara::prelude::*;

// Auto-detect from URL
let config = ResourceConfig::new("https://example.com/song.mp3")?;
let mut resource = Resource::new(config).await?;

let spec = resource.spec();
let mut buf = [0.0f32; 1024];
while !resource.is_eof() {
    let n = resource.read(&mut buf);
    play(&buf[..n]);
}
```

## Architecture

```mermaid
%%{init: {"flowchart": {"curve": "linear"}} }%%
graph TD
    RC[ResourceConfig] -->|auto-detect| R[Resource]
    R -->|".m3u8"| AH["Audio‹Stream‹Hls››"]
    R -->|other| AF["Audio‹Stream‹File››"]

    AH --> PR["Box‹dyn PcmReader›"]
    AF --> PR

    PR -->|"read / seek"| APP[Application]

    AH -. "AudioPipelineEvent‹HlsEvent›" .-> EV[ResourceEvent<br/>broadcast]
    AF -. "AudioPipelineEvent‹FileEvent›" .-> EV
    EV -.-> APP
```

`Resource` wraps `Box<dyn PcmReader>` and spawns tokio tasks to forward typed `AudioPipelineEvent<E>` into unified `ResourceEvent` broadcast channel.

## Features

| Feature | Enables |
|---------|---------|
| `file` | Progressive file download (`kithara-file`) |
| `hls` | HLS streaming + ABR (`kithara-hls`, `kithara-abr`) |
| `rodio` | `rodio::Source` adapter for direct playback |
| `assets` | Re-export `kithara-assets` |
| `net` | Re-export `kithara-net` |
| `bufpool` | Re-export `kithara-bufpool` |

## Integration

Re-exports all sub-crates as modules. The `prelude` module aggregates the most common types: `Resource`, `ResourceConfig`, `Audio`, `AudioConfig`, `Stream`, `PcmReader`, etc.
