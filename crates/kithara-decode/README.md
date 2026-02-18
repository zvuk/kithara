<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-decode.svg)](https://crates.io/crates/kithara-decode)
[![Downloads](https://img.shields.io/crates/d/kithara-decode.svg)](https://crates.io/crates/kithara-decode)
[![docs.rs](https://docs.rs/kithara-decode/badge.svg)](https://docs.rs/kithara-decode)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-decode

Pure audio decoding library built on Symphonia. Provides a synchronous `Decoder` that converts compressed audio (MP3, AAC, FLAC, WAV, etc.) into `PcmChunk` samples (pool-backed `Vec<f32>`). No threading, no channels -- just a thin wrapper over Symphonia's codec pipeline.

## Usage

```rust
use std::io::Cursor;
use kithara_decode::Decoder;
use kithara_bufpool::pcm_pool;

let reader = Cursor::new(wav_bytes);
let mut decoder = Decoder::new_with_probe(reader, None, pcm_pool().clone())?;

let spec = decoder.spec(); // sample_rate, channels
while let Ok(Some(chunk)) = decoder.next_chunk() {
    play(&chunk.pcm);
}
```

## Backends

<table>
<tr><th>Backend</th><th>Implementation</th><th>Platform</th></tr>
<tr><td>Symphonia</td><td>Software decoding; all formats</td><td>Cross-platform</td></tr>
<tr><td>Apple AudioToolbox</td><td>Hardware-accelerated; fMP4, ADTS, MP3, FLAC, CAF</td><td>macOS / iOS</td></tr>
<tr><td>Android MediaCodec</td><td>Placeholder</td><td>Android</td></tr>
</table>

## Initialization Paths

1. **Direct reader creation** (`container` specified): Creates format reader directly without probing. Used for HLS fMP4 where format is known but byte length is unknown. Seek is disabled during init to prevent `IsoMp4Reader` from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports `probe_no_seek` for ABR variant switches where reported byte length may not match.

## Feature Flags

<table>
<tr><th>Feature</th><th>Effect</th></tr>
<tr><td><code>apple</code></td><td>Enables Apple AudioToolbox hardware decoder</td></tr>
<tr><td><code>android</code></td><td>Enables Android MediaCodec decoder (placeholder)</td></tr>
<tr><td><code>perf</code></td><td>Performance instrumentation via <code>hotpath</code></td></tr>
<tr><td><code>test-utils</code></td><td>Mock trait generation via <code>unimock</code></td></tr>
</table>

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
