# `kithara-file` — progressive file download and playback

`kithara-file` provides progressive download and playback capabilities for single-file media resources (MP3, AAC, etc.) in Kithara. It implements the `MediaSource` trait from `kithara-decode` to provide HTTP downloads with seeking, caching, and event-driven playback.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core components
- `struct FileMediaSource` — File source implementing `MediaSource` trait for streaming decode
- `struct File` — Marker type implementing `SourceFactory` trait (legacy)
- `struct SessionSource` — Implementation of `Source` trait for file streams (legacy)
- `struct Progress` — Progress tracker for download and playback positions

### Configuration
- `struct FileParams` — Configuration for file source (cache, network, cancellation)

### Events and errors
- `enum FileEvent` — Events emitted during download/playback
- `enum SourceError` — Error type for source operations

## Architecture overview

`kithara-file` bridges several Kithara crates:

```
┌─────────────────────────────────────────────────────────────┐
│                    kithara-decode                           │
│              (StreamDecoder, AudioSyncReader)               │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                    kithara-file                             │
│      (FileMediaSource implements MediaSource)               │
│      (FileMediaStream implements MediaStream)               │
└─────────────┬─────────────────────────────┬─────────────────┘
              │                             │
┌─────────────▼─────────────┐   ┌──────────▼─────────────┐
│      kithara-net          │   │    kithara-assets      │
│   (HTTP client, Net trait)│   │ (storage, Assets trait)│
└───────────────────────────┘   └────────────────────────┘
```

### Key responsibilities

1. **HTTP progressive download**: Downloads files with range request support
2. **Seek support**: Random access via `MediaStream` Read/Seek traits
3. **Storage integration**: Writes downloaded data to `kithara-assets` storage
4. **Event emission**: Emits progress events via broadcast channel
5. **Session management**: Manages download lifecycle and resource cleanup

## Core invariants

1. **Progressive playback**: Playback can start before download completes
2. **Seek support**: Decoder can seek anywhere; download catches up
3. **Storage persistence**: Downloaded data persists in `kithara-assets` for offline playback
4. **Event-driven**: Emits events for download and playback progress
5. **Cancellation**: All operations respect cancellation tokens

## Usage

### With StreamDecoder (recommended)
```rust
use kithara_decode::{MediaSource, StreamDecoder};
use kithara_file::{FileMediaSource, FileParams, FileEvent};

// Open file media source
let source = FileMediaSource::open(url, FileParams::default()).await?;

// Subscribe to events
let mut events = source.events();
tokio::spawn(async move {
    while let Ok(event) = events.recv().await {
        match event {
            FileEvent::DownloadProgress { offset, percent } => {
                println!("Download: {} bytes ({:?}%)", offset, percent);
            }
            FileEvent::PlaybackProgress { position, percent } => {
                println!("Playback: {} bytes ({:?}%)", position, percent);
            }
        }
    }
});

// Open media stream and create decoder
let stream = source.open()?;
let mut decoder = StreamDecoder::new(stream)?;

// Decode loop
while let Some(chunk) = decoder.decode_next()? {
    // chunk.pcm contains f32 samples
    // chunk.spec contains sample_rate and channels
    play_audio(chunk);
}
```

### With rodio playback
```rust
use kithara_decode::{MediaSource, StreamDecoder, AudioSyncReader, PcmSpec};
use kithara_file::{FileMediaSource, FileParams};

let source = FileMediaSource::open(url, FileParams::default()).await?;
let stream = source.open()?;
let mut decoder = StreamDecoder::new(stream)?;

// Create channel for PCM data
let (tx, rx) = kanal::bounded::<Vec<f32>>(16);

// Decode in background thread
std::thread::spawn(move || {
    while let Ok(Some(chunk)) = decoder.decode_next() {
        if tx.send(chunk.pcm).is_err() {
            break;
        }
    }
});

// Play via rodio
let spec = PcmSpec { sample_rate: 44100, channels: 2 };
let audio_source = AudioSyncReader::new(rx, spec);
let sink = rodio::Sink::connect_new(stream_handle.mixer());
sink.append(audio_source);
sink.sleep_until_end();
```

## Events

### `FileEvent` variants

- `DownloadProgress { offset: u64, percent: Option<f32> }` — Download position update
- `PlaybackProgress { position: u64, percent: Option<f32> }` — Playback position update

## Error handling

### `SourceError` categories

- `Net(NetError)` — Network-related errors
- `Assets(AssetsError)` — Storage-related errors
- `Storage(StorageError)` — Low-level storage errors

## Integration with other Kithara crates

### `kithara-decode`
- `FileMediaSource` implements `MediaSource` trait
- `FileMediaStream` implements `MediaStream` trait
- Works with `StreamDecoder` for PCM output
- `AudioSyncReader` bridges to rodio for playback

### `kithara-net`
- Uses `HttpClient` for HTTP operations
- Leverages range request support for seeking

### `kithara-assets`
- Stores downloaded files as streaming resources
- Uses `StreamingResource` for progressive storage
- Manages asset lifecycle via `AssetStore`

## Design philosophy

1. **Progressive first**: Designed for streaming playback during download
2. **Seek support**: Full seek support for interactive playback
3. **Storage integration**: Tight integration with `kithara-assets` for persistence
4. **Event-driven**: Event system for UI integration
5. **Composable**: Works with other Kithara crates as part of larger system
