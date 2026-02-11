<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

# kithara-file

Progressive file download and playback for single-file media (MP3, AAC, etc.). Implements `StreamType` for use with `Stream<File>`, providing HTTP download with disk caching, seeking, and progress events.

## Usage

```rust
use kithara_stream::Stream;
use kithara_file::{File, FileConfig};

let config = FileConfig::new(url);
let stream = Stream::<File>::new(config).await?;
```

## Download flow

```mermaid
sequenceDiagram
    participant Net as HttpClient
    participant DL as FileDownloader (tokio task)
    participant SR as StorageResource
    participant S as Stream<File> (sync)

    DL->>Net: stream(url)
    Net-->>DL: byte stream

    loop Download chunks
        DL->>SR: write_at(offset, chunk)
        DL->>DL: check backpressure
        Note over DL,S: pause if download_pos - read_pos > look_ahead_bytes
        DL--)S: FileEvent::DownloadProgress
    end

    DL->>SR: commit(total_len)
    DL--)S: FileEvent::DownloadComplete

    S->>SR: wait_range(offset..end)
    Note over SR: blocks until range written
    SR-->>S: data
```

- **Backpressure**: downloader pauses when too far ahead of the reader (configurable `look_ahead_bytes`). Resumes when reader advances (notified via `tokio::Notify`).
- **Lifecycle**: `Backend` task is leaked (`mem::forget`) and runs until cancellation or completion.

## Three-Phase Download

| Phase | Strategy |
|-------|----------|
| Sequential | Stream from file start via `Writer`; fast path for complete downloads |
| Gap Filling | HTTP Range requests for missing chunks; batches up to 4 gaps, each up to 2 MB |
| Complete | All data downloaded; resource committed |

## Local File Handling

When configured with a local path, the crate opens the file via `AssetStore` with an absolute `ResourceKey`, skips all network activity, and creates a fully-cached `FileSource` with no background downloader.

## On-Demand Downloads

When the reader seeks beyond the current download position, it pushes a range request via a lock-free `SegQueue`. The downloader picks up these requests with higher priority than sequential downloading, enabling responsive seek behavior during streaming.

## Integration

Depends on `kithara-net` for HTTP and `kithara-assets` for caching. Composes with `kithara-audio` as `Audio<Stream<File>>` for full decode pipeline.
