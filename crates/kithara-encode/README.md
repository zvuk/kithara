<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![CI](https://github.com/zvuk/kithara/actions/workflows/ci.yml/badge.svg)](https://github.com/zvuk/kithara/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-encode

Synchronous audio encoding library with a thin facade and FFmpeg-backed implementations. `EncoderFactory` creates packaged access units for fMP4/HLS helpers and complete encoded bytes for test fixture routes.

## Usage

```rust
use kithara_encode::{BytesEncodeRequest, BytesEncodeTarget, EncoderFactory};

let encoder = EncoderFactory::create_bytes(BytesEncodeTarget::Mp3)?;
let encoded = encoder.encode_bytes(BytesEncodeRequest {
    pcm: &pcm_source,
    target: BytesEncodeTarget::Mp3,
    bit_rate: None,
})?;
```

## Outputs

- `EncoderFactory::create_bytes` returns `Box<dyn InnerEncoder>` for byte-oriented encoding.
- `EncoderFactory::create_packaged` returns `Box<dyn InnerEncoder>` for packaged encoding.
- `EncoderFactory::encode_bytes` remains as a convenience wrapper that returns `EncodedBytes`.
- `EncoderFactory::encode_packaged` returns `EncodedTrack` with compressed access units for downstream fMP4 muxing.

## Integration

Consumes canonical `AudioCodec`, `ContainerFormat`, and `MediaInfo` from `kithara-stream`. Intended for test infrastructure and used by `kithara-test-utils` for native FFmpeg-backed signal and packaged-audio generation.
