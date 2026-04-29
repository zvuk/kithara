<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/kithara-decode.svg)](https://crates.io/crates/kithara-decode)
[![docs.rs](https://docs.rs/kithara-decode/badge.svg)](https://docs.rs/kithara-decode)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-decode

Audio decoding library with explicit, typed backend selection. `DecoderFactory` creates synchronous `Decoder` instances that convert compressed audio (MP3, AAC, FLAC, WAV, etc.) into `PcmChunk` samples (pool-backed `Vec<f32>`). No threading, no channels — just decoding.

Two traits drive the crate:

- `Decoder` (public) — runtime instance contract: `next_chunk`, `seek`, `spec`, `duration`, `update_byte_len`, `metadata`. Used through `Box<dyn Decoder>` by `kithara-audio` and tests.
- `Backend: Decoder` (`pub(crate)`) — static-dispatch factory: `supports(codec, container) -> bool` plus `try_create(...) -> Self`. Every real backend is also a `Decoder`, so `dispatch::<B>` boxes the result straight into `Box<dyn Decoder>` without juggling associated types.

## Usage

```rust
use std::io::Cursor;
use kithara_decode::{DecoderBackend, DecoderConfig, DecoderFactory};

let reader = Cursor::new(wav_bytes);
let mut config = DecoderConfig::default();
config.backend = DecoderBackend::Symphonia;
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

## Backends

<table>
<tr><th>Backend</th><th>Implementation</th><th>Platform</th></tr>
<tr><td>Symphonia</td><td>Software decoding; all formats</td><td>Cross-platform</td></tr>
<tr><td>Apple AudioToolbox</td><td>Hardware-accelerated; fMP4, ADTS, MP3, FLAC, CAF</td><td>macOS / iOS</td></tr>
<tr><td>Android MediaCodec</td><td>Runtime hardware path for AAC family, MP3, FLAC with recoverable fallback to Symphonia</td><td>Android</td></tr>
</table>

## Initialization Paths

1. **Direct reader creation** (`container` specified): Creates format reader directly without probing. Used for HLS fMP4 where format is known but byte length is unknown. Seek is disabled during init to prevent `IsoMp4Reader` from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports `probe_no_seek` for ABR variant switches where reported byte length may not match.

## Decoder recreate strategy

- `create_for_recreate` is used for seek-time decoder rebuild.
- It is a thin wrapper over `create_from_media_info`: callers must
  supply a `base_offset` that lines up with the container's init
  region (for fMP4/MP4/WAV/MKV/CAF the `ftyp`/RIFF/EBML header; for
  MPEG-ES / ADTS / FLAC / Ogg / MPEG-TS any valid packet start).
- **No fallback**: when the metadata-driven path fails the error is
  propagated verbatim. Probing mid-segment bytes at a mismatched
  offset can silently match an unrelated codec (e.g. MP3 frame sync
  in raw AAC-in-fMP4 bytes) and drive the rest of the pipeline off a
  `session.media_info` the decoder never actually realised.

## Feature Flags

<table>
<tr><th>Feature</th><th>Effect</th></tr>
<tr><td><code>apple</code></td><td>Enables Apple AudioToolbox hardware decoder</td></tr>
<tr><td><code>android</code></td><td>Enables Android MediaCodec hardware backend and fallback plumbing</td></tr>
<tr><td><code>perf</code></td><td>Performance instrumentation via <code>hotpath</code></td></tr>
<tr><td><code>test-utils</code></td><td>Mock trait generation via <code>unimock</code></td></tr>
</table>

## Module layout

- `src/traits.rs` — public `Decoder` runtime trait, plus typed outcomes (`DecoderChunkOutcome`, `DecoderSeekOutcome`, `InputReadOutcome`) and the `DecoderInput` source supertrait.
- `src/backend/` — `pub(crate) trait Backend: Decoder` (capability + factory). One file per concern: `protocol.rs` (the trait), `tests.rs` (capability-only test backends).
- `src/apple/` — `AppleDecoder` (in `decoder.rs`) implements `Decoder`; `backend.rs` carries `impl Backend for AppleDecoder` plus the codec/container helpers (`supports_codec`, `default_container_for_codec`, `can_seek_container`). FFI under `audiofile.rs` / `fmp4.rs` / `converter.rs` / `ffi.rs`.
- `src/android/` — `AndroidDecoder` (`decoder.rs`) implements `Decoder`; `backend.rs` carries `impl Backend for AndroidDecoder`. Whole module gated once in `lib.rs`; no internal `#[cfg(target_os = "android")]`.
- `src/symphonia/` — `SymphoniaDecoder` (`decoder.rs`) implements `Decoder`; `backend.rs` carries `impl Backend for SymphoniaDecoder` plus the `SymphoniaConfig` assembly. `probe.rs` and `adapter.rs` host the probe/direct paths and `ReadSeekAdapter`.
- `src/pcm/` — host-agnostic PCM conversion helpers (`pcm16_to_f32`, `pcm_float_to_pool`) and timeline math (`pcm_meta_from_pts_us`, `seek_trim_for_buffer`) shared across backends.
- `src/factory/` — public `DecoderConfig` + `DecoderFactory` + the `DecoderBackend` enum selector. `dispatch::<B: Backend>` is the single point that boxes the concrete decoder into `Box<dyn Decoder>`.
- `src/hooks.rs` — `HookedDecoder` adapter that wraps any `Box<dyn Decoder>` and forwards `DecoderHooks` callbacks at the chunk/seek boundary.

## Cross-decoder protocol test

`tests/protocol.rs` (integration test) decodes the same MP3 with
every available backend and asserts agreement on `spec()`, `duration()`,
total frame count, post-seek timestamp, EOF semantics, and — when the
`apple` feature is enabled on macOS/iOS — the full-decode PCM L2 norm
within 2 %. Run with:

```
cargo test -p kithara-decode --test protocol --features apple
```

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
