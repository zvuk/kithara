# `kithara-file` — progressive file download and playback

`kithara-file` provides progressive download and playback capabilities for single-file media resources (MP3, AAC, etc.) in Kithara. It implements the `SourceFactory` trait from `kithara-stream` to provide HTTP downloads with seeking, caching, and event-driven playback.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core components
- `struct File` — Marker type implementing `SourceFactory` trait
- `struct SessionSource` — Implementation of `Source` trait for file streams
- `struct Progress` — Progress tracker for download and playback positions

### Configuration
- `struct FileParams` — Configuration for file source (cache, network, cancellation)

### Events and errors
- `enum FileEvent` — Events emitted during download/playback
- `enum SourceError` — Error type for source operations

## Architecture overview

`kithara-file` bridges several Kithara crates:

```
┌─────────────────────────────────────────────────────────┐
│                    kithara-stream                       │
│              (StreamSource<File>, SyncReader)           │
└──────────────────────────┬──────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────┐
│                    kithara-file                         │
│      (File implements SourceFactory)                    │
│      (SessionSource implements Source)                  │
└─────────────┬─────────────────────────────┬─────────────┘
              │                             │
┌─────────────▼─────────────┐   ┌──────────▼─────────────┐
│      kithara-net          │   │    kithara-assets      │
│   (HTTP client, Net trait)│   │ (storage, Assets trait)│
└───────────────────────────┘   └────────────────────────┘
```

### Key responsibilities

1. **HTTP progressive download**: Downloads files with range request support
2. **Seek support**: Random access via `Source::wait_range()` and `read_at()`
3. **Storage integration**: Writes downloaded data to `kithara-assets` storage
4. **Event emission**: Emits progress events via broadcast channel
5. **Session management**: Manages download lifecycle and resource cleanup

## Core invariants

1. **Progressive playback**: Playback can start before download completes
2. **Seek support**: `SyncReader` can seek anywhere; download catches up
3. **Storage persistence**: Downloaded data persists in `kithara-assets` for offline playback
4. **Event-driven**: Emits events for download and playback progress
5. **Cancellation**: All operations respect cancellation tokens

## Usage

### Basic file download and playback
```rust
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use kithara_file::{File, FileParams};

// Open source (async)
let source = StreamSource::<File>::open(url, FileParams::default()).await?;

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

// Create sync reader for decoder
let reader = SyncReader::new(source.into_inner(), SyncReaderParams::default());

// Use with Symphonia or rodio
```

### With rodio (kithara-decode)
```rust
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use kithara_file::{File, FileParams};

let source = StreamSource::<File>::open(url, FileParams::default()).await?;

// Create sync reader for rodio
let reader = SyncReader::new(Arc::new(source), SyncReaderParams::default());

// Play via rodio
let sink = rodio::Sink::connect_new(stream_handle.mixer());
sink.append(rodio::Decoder::new(reader)?);
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

### `kithara-stream`
- `File` implements `SourceFactory`
- `SessionSource` implements `Source`
- Works with `StreamSource<File>` and `SyncReader`

### `kithara-net`
- Uses `HttpClient` for HTTP operations
- Leverages range request support for seeking

### `kithara-assets`
- Stores downloaded files as streaming resources
- Uses `StreamingResource` for progressive storage
- Manages asset lifecycle via `AssetStore`

### `kithara-decode`
- `SyncReader` works with `rodio::Decoder` for playback
- `StreamDecoder` for on-demand decoding with MediaSource

## Design philosophy

1. **Progressive first**: Designed for streaming playback during download
2. **Seek support**: Full seek support for interactive playback
3. **Storage integration**: Tight integration with `kithara-assets` for persistence
4. **Event-driven**: Event system for UI integration
5. **Composable**: Works with other Kithara crates as part of larger system
