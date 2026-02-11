<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
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

| Backend | Implementation | Platform |
|---------|---------------|----------|
| Symphonia | Software decoding; all formats | Cross-platform |
| Apple AudioToolbox | Hardware-accelerated; fMP4, ADTS, MP3, FLAC, CAF | macOS / iOS |
| Android MediaCodec | Placeholder | Android |

## Initialization Paths

1. **Direct reader creation** (`container` specified): Creates format reader directly without probing. Used for HLS fMP4 where format is known but byte length is unknown. Seek is disabled during init to prevent `IsoMp4Reader` from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports `probe_no_seek` for ABR variant switches where reported byte length may not match.

## Feature Flags

| Feature | Effect |
|---------|--------|
| `apple` | Enables Apple AudioToolbox hardware decoder |
| `android` | Enables Android MediaCodec decoder (placeholder) |
| `perf` | Performance instrumentation via `hotpath` |
| `test-utils` | Mock trait generation via `unimock` |

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
