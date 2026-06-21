<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

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

## Key types

- `EncoderFactory` — entry point; creates byte-oriented and packaged encoders.
- `InnerEncoder` — encoder trait returned by the factory.
- `BytesEncodeRequest` / `BytesEncodeTarget` — byte-encoding inputs.
- `EncodedBytes` / `EncodedTrack` — encoded outputs (complete bytes and packaged access units).

Consumes canonical `AudioCodec`, `ContainerFormat`, and `MediaInfo` from `kithara-stream`. Intended for test infrastructure and used by `kithara-test-utils` for native FFmpeg-backed signal and packaged-audio generation.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
