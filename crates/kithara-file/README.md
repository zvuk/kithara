# `kithara-file` — progressive file download and playback

`kithara-file` provides progressive download and playback capabilities for single-file media resources (MP3, AAC, etc.) in Kithara. It implements the `EngineSource` contract from `kithara-stream` to orchestrate HTTP downloads with support for seeking, caching, and event-driven playback.

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core components
- `struct FileSource` — Implementation of `EngineSource` for progressive file downloads
- `struct FileSession` — Active download session with control interface
- `struct FileSourceContract` — Builder for creating `FileSource` instances

### Configuration and options
- `struct FileSourceOptions` — Configuration for file source behavior
- `enum OptionsError` — Error type for option validation

### Events and errors
- `enum FileEvent` — Events emitted during file download/playback
- `enum FileError` — Error type for file operations
- `type FileResult<T> = Result<T, FileError>` — Result alias
- `enum DriverError` — Error type for driver operations
- `enum SourceError` — Error type for source operations

## Architecture overview

`kithara-file` bridges several Kithara crates:

```
┌─────────────────────────────────────────────────────────┐
│                    kithara-stream                       │
│                    (Engine/EngineSource)                │
└──────────────────────────┬──────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────┐
│                    kithara-file                         │
│      (FileSource implements EngineSource)               │
└─────────────┬─────────────────────────────┬─────────────┘
              │                             │
┌─────────────▼─────────────┐   ┌──────────▼─────────────┐
│      kithara-net          │   │    kithara-assets      │
│   (HTTP client, Net trait)│   │ (storage, Assets trait)│
└───────────────────────────┘   └────────────────────────┘
```

### Key responsibilities

1. **HTTP progressive download**: Downloads files with range request support
2. **Seek coordination**: Handles seek commands by restarting downloads at new positions
3. **Storage integration**: Writes downloaded data to `kithara-assets` storage
4. **Event emission**: Emits progress, completion, and error events
5. **Session management**: Manages download lifecycle and resource cleanup

## Core invariants

1. **Progressive playback**: Playback can start before download completes
2. **Seek support**: Seek commands restart download from new position (if server supports ranges)
3. **Storage persistence**: Downloaded data persists in `kithara-assets` for offline playback
4. **Event-driven**: Emits events for download progress, completion, and errors
5. **Cancellation**: All operations respect cancellation tokens
6. **Error recovery**: Implements retry logic for transient network failures

## `FileSource` as `EngineSource`

`FileSource` implements the `EngineSource` trait from `kithara-stream`, providing:

- **Byte stream orchestration**: Coordinates HTTP download with storage writing
- **Seek handling**: Restarts download from seek position when requested
- **Event mapping**: Maps download progress to `FileEvent` types
- **Error propagation**: Converts `NetError` to `FileError` with context

### Implementation details

```rust
#[async_trait::async_trait]
impl EngineSource for FileSource {
    type Error = FileError;
    type Control = FileSession;
    type Event = FileEvent;

    async fn open(
        &self,
        params: StreamParams,
    ) -> Result<(WriterTask<Self::Control, Self::Event, Self::Error>, Self::Control), StreamError<Self::Error>> {
        // Creates download session and returns control handle
    }

    async fn seek_bytes(
        &self,
        pos: u64,
        control: &mut Self::Control,
    ) -> Result<(), StreamError<Self::Error>> {
        // Restarts download from new position
    }

    fn supports_seek(&self) -> bool {
        // Returns true if server supports range requests
    }
}
```

## `FileSession` control interface

`FileSession` provides control over an active download:

- **Progress monitoring**: Query download progress and status
- **Error handling**: Access download errors if they occur
- **Resource management**: Manages asset resources and cleanup
- **Event subscription**: Receive download events

## Configuration

### `FileSourceOptions`

- `url: String` — URL of the file to download
- `asset_id: Option<AssetId>` — Optional asset ID for storage (auto-generated if None)
- `offline_mode: bool` — Whether to operate in offline mode (no network)
- `retry_policy: RetryPolicy` — Retry policy for network failures
- `connect_timeout: Duration` — Connection timeout
- `read_timeout: Duration` — Read timeout

## Events

### `FileEvent` variants

- `Progress { downloaded: u64, total: Option<u64> }` — Download progress update
- `Complete` — Download completed successfully
- `Error(FileError)` — Download failed with error
- `SeekStarted { position: u64 }` — Seek operation started
- `SeekCompleted { position: u64 }` — Seek operation completed

Events are emitted through the `Engine` stream as `StreamMsg::Event(FileEvent)`.

## Error handling

### `FileError` categories

- `Network(NetError)` — Network-related errors (connection, timeout, HTTP)
- `Storage(AssetsError)` — Storage-related errors (disk full, permission denied)
- `InvalidUrl` — Malformed or unsupported URL
- `InvalidRange` — Invalid byte range requested
- `Cancelled` — Operation was cancelled
- `Other` — Other unexpected errors

## Integration with other Kithara crates

### `kithara-stream`
- `FileSource` implements `EngineSource`
- Uses `Engine` for orchestration
- Emits events through `StreamMsg::Event`

### `kithara-net`
- Uses `Net` trait for HTTP operations
- Leverages range request support for seeking
- Uses retry and timeout policies

### `kithara-assets`
- Stores downloaded files as assets
- Uses `StreamingResource` for progressive storage
- Manages asset lifecycle and cleanup

### `kithara-decode`
- Can decode downloaded audio files
- Uses the same asset storage for playback

## Example usage

### Basic file download and playback
```rust
use kithara_file::{FileSource, FileSourceContract, FileSourceOptions};
use kithara_stream::{Engine, StreamParams};
use std::time::Duration;

// Create file source
let options = FileSourceOptions {
    url: "https://example.com/audio.mp3".to_string(),
    asset_id: None, // Auto-generate asset ID
    offline_mode: false,
    retry_policy: Default::default(),
    connect_timeout: Duration::from_secs(10),
    read_timeout: Duration::from_secs(30),
};

let source = FileSourceContract::new(options).build();

// Create and run engine
let (engine, handle, stream) = Engine::build(source, StreamParams::default());
tokio::spawn(engine.run());

// Process stream
while let Some(msg) = stream.next().await {
    match msg {
        Ok(StreamMsg::Data(bytes)) => {
            // Process audio data
        }
        Ok(StreamMsg::Event(event)) => {
            match event {
                FileEvent::Progress { downloaded, total } => {
                    println!("Progress: {}/{:?}", downloaded, total);
                }
                FileEvent::Complete => {
                    println!("Download complete");
                }
                FileEvent::Error(e) => {
                    eprintln!("Download error: {:?}", e);
                }
                _ => {}
            }
        }
        Err(e) => {
            eprintln!("Stream error: {:?}", e);
        }
    }
}
```

### Seeking during playback
```rust
// Seek to 30 seconds into the file (assuming 128kbps MP3)
let bytes_per_second = 16000; // Approximate for 128kbps
let seek_position = 30 * bytes_per_second;

if let Err(e) = handle.seek_bytes(seek_position).await {
    eprintln!("Seek failed: {:?}", e);
}
```

### Offline mode
```rust
let options = FileSourceOptions {
    url: "https://example.com/audio.mp3".to_string(),
    asset_id: Some(existing_asset_id),
    offline_mode: true, // Only read from cache, no network
    ..Default::default()
};

let source = FileSourceContract::new(options).build();
// Will fail if asset doesn't exist in cache
```

## Design philosophy

1. **Progressive first**: Designed for streaming playback during download
2. **Seek support**: Full seek support for interactive playback
3. **Storage integration**: Tight integration with `kithara-assets` for persistence
4. **Event-driven**: Rich event system for UI integration
5. **Error resilience**: Robust error handling and recovery
6. **Composable**: Works with other Kithara crates as part of larger system