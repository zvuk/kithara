# kithara-encode — Context

Detailed contracts and invariants for the kithara-encode crate; the README is the overview.

## Outputs

- `EncoderFactory::create_bytes` returns `Box<dyn InnerEncoder>` for byte-oriented encoding.
- `EncoderFactory::create_packaged` returns `Box<dyn InnerEncoder>` for packaged encoding.
- `EncoderFactory::encode_bytes` remains as a convenience wrapper that returns `EncodedBytes`.
- `EncoderFactory::encode_packaged` returns `EncodedTrack` with compressed access units for downstream fMP4 muxing.

## Integration

Consumes canonical `AudioCodec`, `ContainerFormat`, and `MediaInfo` from `kithara-stream`. Intended for test infrastructure and used by `kithara-test-utils` for native FFmpeg-backed signal and packaged-audio generation.
