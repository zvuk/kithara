# kithara-file

HTTP file streaming source for Kithara with lazy caching and seek support.

## Purpose

`kithara-file` provides streaming access to individual files via HTTP with:
- Progressive download with backpressure
- Optional persistent caching
- Seek support via HTTP Range requests
- Driver/session separation for clean async/sync boundaries

## Public Contract

### Core Types

- `FileSource::open(url, options)` - Create session
- `FileSource::open_with_cache(url, options, cache)` - Create session with caching
- `FileSession` - Handle to active stream with:
  - `asset_id()` - Stable asset identifier
  - `stream()` - Async bytes stream  
  - `seek_bytes(position)` - Seek command (best-effort)
  - `stop()` - Stop command

### Seek Contract

Seek operations are **best-effort** and **absolute**:

1. **Range Policy**: When `enable_range_seek=true`, seeks use HTTP Range headers
2. **Validation**: Positions beyond file size return `InvalidSeekPosition` error
3. **Behavior**:
   - `< 0`: Clamped to 0
   - `> file_size`: Returns error (or clamped if file size unknown)
   - Repeated seeks: Safe and idempotent
4. **Stream Reset**: Seek may reset/rewind the underlying stream

### Cache Interaction

Caching behavior when `AssetCache` provided:

1. **Cache-First**: Read from cache before network
2. **Cache-Through**: Stream data while simultaneously writing to cache
3. **Atomic Write**: Cache written atomically (`tmp -> rename`)
4. **Offline Playback**: Cache serves data when network unavailable
5. **Cache Layout**: Files stored at `file/body` within asset directory

### Error Handling

- **Recoverable**: Temporary network errors (retries attempted)
- **Fatal**: Invalid URLs, cache errors, seek not supported
- **Stream Termination**: Fatal errors end the stream cleanly via `finish()`

## Internal Architecture

The crate is organized into focused modules:

- **`driver/`**: FileDriver async loop and stream logic
- **`session/`**: Session handle and command interface  
- **`options/`**: Configuration and validation
- **`range_policy/`**: Seek validation and range request logic

## Usage Example

```rust
use kithara_file::{FileSource, FileSourceOptions};
use futures::StreamExt;

let url = "https://example.com/audio.mp3".parse()?;
let options = FileSourceOptions::new().with_range_seek(true);

let session = FileSource::open(url, options).await?;
let mut stream = session.stream();

while let Some(chunk) = stream.next().await {
    let bytes = chunk?;
    // Process audio bytes
}

// Seek to position
session.seek_bytes(1024)?;
```