<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-decode

Audio decoding library built on Symphonia. Provides generic `Decoder<S>` running in a blocking thread with epoch-based format change detection, PCM output via channel, and optional rodio integration. Supports MP3, AAC, FLAC, and other Symphonia-supported formats.

## Usage

```rust
use kithara_decode::{Decoder, DecoderConfig};
use kithara_hls::{Hls, HlsConfig};
use kithara_stream::Stream;

let config = DecoderConfig::<Hls>::new(hls_config).streaming();
let decoder = Decoder::<Stream<Hls>>::new(config).await?;

// Read PCM chunks
while let Ok(chunk) = decoder.pcm_rx().recv() {
    play_audio(chunk);
}
```

## Integration

Consumes `Stream<T>` from `kithara-stream`. Works with both `Stream<File>` and `Stream<Hls>`. Emits `DecoderEvent<E>` combining stream and decode events. With the `rodio` feature, provides `AudioSyncReader` as a `rodio::Source` adapter.
