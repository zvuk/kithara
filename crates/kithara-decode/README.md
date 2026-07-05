<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-decode.svg)](https://crates.io/crates/kithara-decode)
[![docs.rs](https://docs.rs/kithara-decode/badge.svg)](https://docs.rs/kithara-decode)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-decode

Audio decoding library with explicit, typed backend selection. `DecoderFactory` creates synchronous `Decoder` instances that convert compressed audio (MP3, AAC, FLAC, WAV, ALAC, …) into pool-backed PCM (`Vec<f32>`). No threading, no channels — just decoding.

The public surface centres on one trait — `Decoder`. Concrete backends (Symphonia / Apple / Android) implement it directly. Internally, container parsing and frame decoding are split: the `Demuxer` trait owns container framing, the `FrameCodec` trait owns codec decoding, and `ComposedDecoder<D, C>` (internal) pairs them so backends can be mixed and matched. The factory hides this detail — callers only ever see `Box<dyn Decoder>`.

## Usage

```rust
use std::io::Cursor;
use kithara_decode::{DecoderBackend, DecoderConfig, DecoderFactory};

let reader = Cursor::new(wav_bytes);
let config = DecoderConfig { backend: DecoderBackend::Symphonia, ..Default::default() };
let mut decoder = DecoderFactory::create_with_probe(reader, Some("wav"), config)?;

let spec = decoder.spec(); // sample_rate, channels
loop {
    match decoder.next_chunk()? {
        kithara_decode::DecoderChunkOutcome::Chunk(chunk) => play(&chunk.pcm),
        kithara_decode::DecoderChunkOutcome::Pending(_) => continue,
        kithara_decode::DecoderChunkOutcome::Eof => break,
    }
}
```

For HLS / cross-codec recreate paths, prefer `DecoderFactory::create_from_media_info(reader, &media_info, config)` — it skips probing and uses the carried `MediaInfo` to pick the backend.

## Backends

<table>
<tr><th>Backend</th><th>Implementation</th><th>Platform</th></tr>
<tr><td>Symphonia</td><td>Software decoding; all formats</td><td>Cross-platform</td></tr>
<tr><td>Apple AudioToolbox</td><td>Hardware-accelerated; fMP4, ADTS, MP3, FLAC, CAF</td><td>macOS / iOS</td></tr>
<tr><td>Android MediaCodec</td><td>Hardware path for fMP4 AAC-LC/FLAC plus standalone WAV, MP3, and ALAC through <code>AMediaExtractor</code>; no runtime Symphonia fallback</td><td>Android</td></tr>
</table>

## Integration

`kithara-audio` owns threading, effects, and resampling; this crate stays a synchronous decoder over `R: Read + Seek + Send + Sync + 'static` inputs such as `Stream<File>`, `Stream<Hls>`, cursors, or plain files.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
