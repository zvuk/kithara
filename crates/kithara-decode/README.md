# `kithara-decode` — audio decoding library for Kithara

`kithara-decode` provides generic audio decoding capabilities for Kithara, built on top of Symphonia. It supports multiple audio formats (MP3, AAC, FLAC, etc.) with flexible sample type handling and both synchronous and asynchronous APIs.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core abstractions
- `trait AudioSource` — Source of audio data for decoding
- `trait MediaSource` — Media source with metadata and seek support
- `trait ReadSeek` — Read + Seek interface for media sources

### Decoder types
- `struct Decoder<T>` — Synchronous decoder with generic sample type `T`
- `struct AudioStream<T>` — Asynchronous decoder stream with backpressure
- `struct DecodeEngine<T>` — Low-level decode engine (advanced use)

### Configuration and types
- `struct DecoderSettings` — Configuration for decoder behavior
- `struct PcmChunk<T>` — Decoded PCM audio chunk
- `struct PcmSpec` — PCM specification (channels, sample rate)
- `struct AudioSpec` — Audio specification with format information
- `enum DecodeCommand` — Control commands for decoder (Seek)

### Error handling
- `enum DecodeError` — Error type for decode operations
- `type DecodeResult<T> = Result<T, DecodeError>` — Result alias

### Sample type traits
- `DaspSample` — Re-export from `dasp::sample::Sample`
- `SymphoniaSample` — Re-export from `symphonia::core::audio::sample::Sample`
- `ConvertibleSample` — Re-export from `symphonia::core::audio::conv::ConvertibleSample`

## Supported formats

Via Symphonia with the "all" feature:
- MP3 (MPEG 1/2 Layer 3)
- AAC (Advanced Audio Coding)
- FLAC (Free Lossless Audio Codec)
- Vorbis (Ogg Vorbis)
- Opus (Ogg Opus)
- WAV (PCM, ADPCM, IEEE Float)
- ALAC (Apple Lossless Audio Codec)
- MP4/M4A (AAC, ALAC)
- And more via Symphonia's format support

## Sample type generics

The library is generic over sample type `T`, which must implement:
- `dasp::sample::Sample` — For sample manipulation
- `symphonia::core::audio::sample::Sample` — For Symphonia integration
- `symphonia::core::audio::conv::ConvertibleSample` — For format conversion
- `Send + 'static` — For threading support

Common choices:
- `f32` — 32-bit float (recommended for processing)
- `i16` — 16-bit integer (common in audio files)
- `u8` — 8-bit unsigned (rare, but supported)

## Core invariants

### For `PcmChunk<T>`:
1. **Frame alignment**: `pcm.len() % channels == 0`
2. **Valid specs**: `channels > 0` and `sample_rate > 0`
3. **Interleaved layout**: Samples stored as LRLRLR... for stereo
4. **Immutable**: Once created, chunks cannot be modified

### For `Decoder<T>`:
1. **Synchronous**: Blocking decode operations
2. **Thread-safe**: Can be sent between threads
3. **Seek support**: Best-effort seeking with `DecodeCommand::Seek`
4. **Error recovery**: Continues decoding after recoverable errors

### For `AudioStream<T>`:
1. **Async-first**: Designed for async/await usage
2. **Backpressure**: Bounded buffer prevents unbounded memory growth
3. **Cancellation**: Respects async cancellation
4. **Command channel**: Accepts `DecodeCommand` for control

## Architecture overview

```
┌─────────────────────────────────────────┐
│          AudioStream<T>                 │
│    (async stream with backpressure)     │
└───────────────────┬─────────────────────┘
                    │
┌───────────────────▼─────────────────────┐
│            Decoder<T>                   │
│      (synchronous decoder wrapper)      │
└───────────────────┬─────────────────────┘
                    │
┌───────────────────▼─────────────────────┐
│          DecodeEngine<T>                │
│    (low-level Symphonia integration)    │
└───────────────────┬─────────────────────┘
                    │
┌───────────────────▼─────────────────────┐
│          Symphonia backend              │
│    (format detection, codec, decode)    │
└─────────────────────────────────────────┘
```

## Integration with other Kithara crates

### `kithara-file` / `kithara-hls`
- Provide `AudioSource` implementations for network streams
- Use `kithara-storage` resources as input sources
- Integrate with `kithara-stream` orchestration

### `kithara-storage`
- `StreamingResource` implements `ReadSeek` for decoder input
- Provides persistent storage for audio files

### `kithara-stream`
- `Reader` can be adapted to `AudioSource`
- Orchestrates download → decode pipeline

## Example usage

### Synchronous decoding
```rust
use kithara_decode::{Decoder, DecoderSettings, AudioSource};
use std::time::Duration;

// Create an AudioSource (e.g., from file or network)
let source: Box<dyn AudioSource> = /* ... */;

// Create decoder with f32 samples
let settings = DecoderSettings::default();
let mut decoder = Decoder::<f32>::new(source, settings)?;

// Decode chunks synchronously
while let Some(chunk) = decoder.next()? {
    println!("Decoded {} frames at {} Hz", 
             chunk.frames(), chunk.spec().sample_rate);
    
    // Process PCM data
    let pcm_data = chunk.pcm();
    let spec = chunk.spec();
}

// Seek to 30 seconds
decoder.send_command(DecodeCommand::Seek(Duration::from_secs(30)))?;
```

### Asynchronous decoding with AudioStream
```rust
use kithara_decode::{AudioStream, DecoderSettings, DecodeCommand};
use std::time::Duration;

// Create audio stream with buffer capacity
let mut stream = AudioStream::<f32>::new(source, 10)?; // 10 chunk buffer

// Send seek command asynchronously
stream.send_command(DecodeCommand::Seek(Duration::from_secs(30))).await?;

// Process chunks asynchronously
while let Some(chunk) = stream.next_chunk().await? {
    // Process audio chunk
    process_audio(chunk.pcm(), chunk.spec());
}

// Get stream statistics
let stats = stream.stats();
println!("Decoded {} frames, {} errors", stats.frames_decoded, stats.decode_errors);
```

### Implementing `AudioSource`
```rust
use kithara_decode::{AudioSource, DecodeError};
use bytes::Bytes;
use std::io::{Read, Seek, SeekFrom};

struct MyAudioSource {
    // Your data source
}

impl AudioSource for MyAudioSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DecodeError> {
        // Read data into buffer
        Ok(bytes_read)
    }

    fn seek(&mut self, pos: SeekFrom) -> Result<u64, DecodeError> {
        // Seek to position
        Ok(new_position)
    }

    fn stream_len(&mut self) -> Result<u64, DecodeError> {
        // Return total stream length if known
        Ok(total_length)
    }
}
```

### Using with `kithara-storage` resources
```rust
use kithara_decode::{Decoder, DecoderSettings};
use kithara_storage::StreamingResource;
use std::sync::Arc;

// Create a storage resource
let resource: Arc<StreamingResource> = /* ... */;

// Wrap it as an AudioSource
let source = StorageAudioSource::new(resource);

// Decode from storage
let mut decoder = Decoder::<f32>::new(Box::new(source), DecoderSettings::default())?;
```

## Error handling

### `DecodeError` categories
- `Io` — I/O errors from the source
- `Decode` — Audio decoding errors
- `Format` — Unsupported or invalid audio format
- `Seek` — Seek operation failed
- `InvalidState` — Decoder in invalid state
- `Cancelled` — Operation was cancelled
- `Other` — Other unexpected errors

## Configuration

### `DecoderSettings`
- `preferred_sample_type: Option<SampleType>` — Preferred sample type for output
- `decode_buffer_frames: usize` — Number of frames per decode operation
- `enable_cga: bool` — Enable channel gain adjustment (if supported)
- `enable_dither: bool` — Enable dithering (if supported)

## Performance considerations

1. **Sample type choice**: `f32` is recommended for processing, `i16` for storage
2. **Buffer sizing**: Larger buffers reduce overhead but increase latency
3. **Chunk size**: Balance between decode granularity and per-chunk overhead
4. **Async vs sync**: Use `AudioStream` for async contexts, `Decoder` for sync

## Design philosophy

1. **Generic over sample type**: Support multiple numeric representations
2. **Dual API**: Both synchronous (`Decoder`) and asynchronous (`AudioStream`) interfaces
3. **Trait-based sources**: Flexible input source abstraction
4. **Error resilience**: Continue decoding after recoverable errors
5. **Seek support**: Best-effort seeking with clear semantics
6. **Backpressure**: Async stream prevents unbounded memory growth
7. **Format agnostic**: Support all formats Symphonia supports
8. **Thread-safe**: All types are `Send + Sync` where appropriate