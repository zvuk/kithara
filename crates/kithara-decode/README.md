<div align="center">
  <img src="../../logo.png" alt="kithara" width="300">
</div>

# kithara-decode

Pure audio decoding library built on Symphonia. Provides a synchronous `Decoder` that converts compressed audio (MP3, AAC, FLAC, WAV, etc.) into `PcmChunk<f32>` samples. No threading, no channels -- just a thin wrapper over Symphonia's codec pipeline.

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

## Integration

Consumed by `kithara-audio` which wraps it in a threaded pipeline with effects and resampling. Accepts any `R: Read + Seek + Send + Sync + 'static` -- works with `Stream<File>`, `Stream<Hls>`, `Cursor<Vec<u8>>`, or plain files.
