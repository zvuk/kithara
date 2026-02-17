<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/kithara-file.svg)](https://crates.io/crates/kithara-file)
[![Downloads](https://img.shields.io/crates/d/kithara-file.svg)](https://crates.io/crates/kithara-file)
[![docs.rs](https://docs.rs/kithara-file/badge.svg)](https://docs.rs/kithara-file)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

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
%%{init: {"flowchart": {"curve": "linear"}} }%%
graph LR
    subgraph Network
        HTTP["HTTP Server"]
    end

    subgraph "Async (tokio)"
        HEAD["HEAD request<br/><i>Content-Length</i>"]
        GET["GET stream<br/><i>byte stream</i>"]
        Writer["Writer&lt;NetError&gt;<br/><i>byte pump</i>"]
        RangeReq["GET Range<br/><i>gap filling</i>"]
    end

    subgraph "Storage (shared)"
        SR["StorageResource<br/><i>Mmap or Mem</i>"]
        RS["RangeSet&lt;u64&gt;<br/><i>available ranges</i>"]
    end

    subgraph "Sync (rayon thread)"
        FS["FileSource<br/><i>Source impl</i>"]
        Reader["Reader&lt;FileSource&gt;<br/><i>Read + Seek</i>"]
        StreamW["Stream&lt;File&gt;"]
    end

    subgraph "Decode (rayon thread)"
        Decoder["Symphonia<br/><i>InnerDecoder</i>"]
        PCM["PcmChunk<br/><i>f32 interleaved</i>"]
    end

    subgraph "Consumer"
        Audio["Audio&lt;Stream&lt;File&gt;&gt;<br/><i>PcmReader</i>"]
    end

    HTTP --> HEAD
    HTTP --> GET
    HTTP --> RangeReq
    GET --> Writer
    RangeReq --> Writer
    Writer -- "write_at(offset, bytes)" --> SR
    SR -- "updates" --> RS
    FS -- "wait_range()" --> SR
    FS -- "read_at()" --> SR
    Reader --> FS
    StreamW --> Reader
    Decoder -- "Read + Seek" --> StreamW
    Decoder --> PCM
    PCM -- "kanal channel" --> Audio

    style SR fill:#d4a574,color:#000
    style RS fill:#d4a574,color:#000
```

- **Backpressure**: downloader pauses when too far ahead of the reader (configurable `look_ahead_bytes`). Resumes when reader advances (notified via `tokio::Notify`).
- **Lifecycle**: `Backend` task is leaked (`mem::forget`) and runs until cancellation or completion.

## Three-Phase Download

<table>
<tr><th>Phase</th><th>Strategy</th></tr>
<tr><td>Sequential</td><td>Stream from file start via <code>Writer</code>; fast path for complete downloads</td></tr>
<tr><td>Gap Filling</td><td>HTTP Range requests for missing chunks; batches up to 4 gaps, each up to 2 MB</td></tr>
<tr><td>Complete</td><td>All data downloaded; resource committed</td></tr>
</table>

## Local File Handling

When configured with a local path, the crate opens the file via `AssetStore` with an absolute `ResourceKey`, skips all network activity, and creates a fully-cached `FileSource` with no background downloader.

## On-Demand Downloads

When the reader seeks beyond the current download position, it pushes a range request via a lock-free `SegQueue`. The downloader picks up these requests with higher priority than sequential downloading, enabling responsive seek behavior during streaming.

## Integration

Depends on `kithara-net` for HTTP and `kithara-assets` for caching. Composes with `kithara-audio` as `Audio<Stream<File>>` for full decode pipeline.
