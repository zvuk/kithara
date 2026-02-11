# Kithara: Codebase Analysis

> This document is produced by analyzing the source code exclusively, without relying on
> README files or other documentation. It describes each crate and the project as a whole.

## Table of Contents

- [Project Overview](#project-overview)
- [Architecture](#architecture)
- [Crate Dependency Graph](#crate-dependency-graph)
- [Diagrams](#diagrams)
  - [Crate Dependencies (Mermaid)](#crate-dependencies-mermaid)
  - [Progressive File Dataflow](#progressive-file-dataflow)
  - [HLS Dataflow](#hls-dataflow)
  - [Thread and Task Model](#thread-and-task-model)
  - [Async-to-Sync Bridge (Storage)](#async-to-sync-bridge-storage)
  - [Assets Decorator Chain](#assets-decorator-chain)
  - [Net Decorator Chain](#net-decorator-chain)
  - [ABR Decision Flow](#abr-decision-flow)
  - [Event Propagation](#event-propagation)
  - [Buffer Pool Lifecycle](#buffer-pool-lifecycle)
  - [HLS Segment Stitching](#hls-segment-stitching)
  - [File Three-Phase Download](#file-three-phase-download)
  - [Seek Sequence (File)](#seek-sequence-file)
  - [HLS ABR Variant Switch](#hls-abr-variant-switch)
- [kithara (facade)](#kithara-facade)
- [kithara-bufpool](#kithara-bufpool)
- [kithara-storage](#kithara-storage)
- [kithara-assets](#kithara-assets)
- [kithara-net](#kithara-net)
- [kithara-stream](#kithara-stream)
- [kithara-file](#kithara-file)
- [kithara-hls](#kithara-hls)
- [kithara-abr](#kithara-abr)
- [kithara-drm](#kithara-drm)
- [kithara-decode](#kithara-decode)
- [kithara-audio](#kithara-audio)
- [Integration Tests](#integration-tests)
- [Cross-Cutting Patterns](#cross-cutting-patterns)

---

## Project Overview

Kithara is a Rust workspace providing networking and decoding building blocks for audio
playback. It is **not** a full audio player; it is a library that handles everything from
fetching audio data (progressive HTTP download or HLS) through decoding to producing PCM
samples ready for playback.

Key capabilities:

- Progressive HTTP file streaming (MP3, AAC, FLAC, WAV, OGG, etc.)
- HLS VOD streaming with adaptive bitrate (ABR)
- AES-128-CBC segment decryption
- Multi-backend audio decoding (Symphonia software, Apple AudioToolbox hardware, Android
  MediaCodec placeholder)
- Sample rate conversion and resampling
- Disk and in-memory caching with LRU eviction
- Zero-allocation hot paths via sharded buffer pools
- Crash-safe partial download tracking

The workspace targets `edition = "2024"` and is dual-licensed under MIT / Apache-2.0.

---

## Architecture

The system is layered bottom-up: low-level storage primitives at the base, protocol
orchestration in the middle, and decoding/audio pipeline at the top. A single facade crate
(`kithara`) unifies everything behind a type-erased `Resource` that can play any supported
source.

---

## Crate Dependency Graph

```mermaid
graph TD
    kithara["kithara<br/><i>facade</i>"]
    audio["kithara-audio<br/><i>pipeline, resampling</i>"]
    decode["kithara-decode<br/><i>Symphonia, Apple, Android</i>"]
    file["kithara-file<br/><i>progressive download</i>"]
    hls["kithara-hls<br/><i>HLS VOD</i>"]
    abr["kithara-abr<br/><i>adaptive bitrate</i>"]
    drm["kithara-drm<br/><i>AES-128-CBC</i>"]
    net["kithara-net<br/><i>HTTP + retry</i>"]
    assets["kithara-assets<br/><i>cache, lease, evict</i>"]
    stream["kithara-stream<br/><i>Source, Writer, Backend</i>"]
    storage["kithara-storage<br/><i>mmap / mem</i>"]
    bufpool["kithara-bufpool<br/><i>sharded pool</i>"]

    kithara --> audio
    kithara --> decode
    kithara --> file
    kithara --> hls

    audio --> decode
    audio --> stream
    audio --> file
    audio --> hls
    audio --> bufpool

    decode --> stream
    decode --> bufpool

    file --> stream
    file --> net
    file --> assets
    file --> storage

    hls --> stream
    hls --> net
    hls --> assets
    hls --> storage
    hls --> abr
    hls --> drm

    assets --> storage
    assets --> bufpool

    stream --> storage
    stream --> bufpool
    stream --> net

    style kithara fill:#4a6fa5,color:#fff
    style audio fill:#6b8cae,color:#fff
    style decode fill:#6b8cae,color:#fff
    style file fill:#7ea87e,color:#fff
    style hls fill:#7ea87e,color:#fff
    style abr fill:#c4a35a,color:#fff
    style drm fill:#c4a35a,color:#fff
    style net fill:#b07a5b,color:#fff
    style assets fill:#b07a5b,color:#fff
    style stream fill:#8b6b8b,color:#fff
    style storage fill:#8b6b8b,color:#fff
    style bufpool fill:#8b6b8b,color:#fff
```

---

## Diagrams

### Crate Dependencies (Mermaid)

See [Crate Dependency Graph](#crate-dependency-graph) above.

---

### Progressive File Dataflow

End-to-end data flow for `Stream<File>` (progressive HTTP download to PCM output):

```mermaid
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

---

### HLS Dataflow

End-to-end data flow for `Stream<Hls>` (HLS VOD with ABR):

```mermaid
graph LR
    subgraph Network
        MasterPL["Master<br/>Playlist"]
        MediaPL["Media<br/>Playlist"]
        SegSrv["Segment<br/>Server"]
        KeySrv["Key<br/>Server"]
    end

    subgraph "Async (tokio)"
        FM["FetchManager<br/><i>fetch + cache</i>"]
        KM["KeyManager<br/><i>key resolution</i>"]
        W["Writer<br/><i>byte pump</i>"]
    end

    subgraph "Downloader (rayon)"
        DL["HlsDownloader<br/><i>plan, commit</i>"]
        ABR["AbrController<br/><i>variant select</i>"]
    end

    subgraph "Assets"
        AB["AssetsBackend<br/><i>Disk or Mem</i>"]
        Proc["ProcessingAssets<br/><i>AES decrypt</i>"]
    end

    subgraph "Sync (rayon thread)"
        HS["HlsSource<br/><i>Source impl</i>"]
        SI["SegmentIndex<br/><i>virtual stream</i>"]
        StreamH["Stream&lt;Hls&gt;"]
    end

    subgraph "Decode (rayon thread)"
        Dec["Symphonia<br/><i>InnerDecoder</i>"]
        PCM2["PcmChunk"]
    end

    subgraph "Consumer"
        Audio2["Audio&lt;Stream&lt;Hls&gt;&gt;"]
    end

    MasterPL --> FM
    MediaPL --> FM
    SegSrv --> FM
    KeySrv --> KM
    KM --> FM
    FM --> W
    W --> AB
    AB --> Proc
    DL -- "plan()" --> FM
    DL -- "commit()" --> SI
    ABR -- "decide()" --> DL
    DL -- "throughput sample" --> ABR
    HS -- "wait on condvar" --> SI
    HS -- "read_at()" --> AB
    StreamH --> HS
    Dec -- "Read + Seek" --> StreamH
    Dec --> PCM2
    PCM2 -- "kanal channel" --> Audio2

    style SI fill:#d4a574,color:#000
    style AB fill:#d4a574,color:#000
```

---

### Thread and Task Model

Overview of threads, async tasks, and their communication channels:

```mermaid
graph TB
    subgraph "Main / Consumer Thread"
        App["Application Code"]
        Resource["Resource / Audio&lt;S&gt;<br/><i>PcmReader interface</i>"]
        App -- "read(buf)" --> Resource
    end

    subgraph "tokio Runtime (async tasks)"
        NetTask["Network I/O<br/><i>reqwest + retry</i>"]
        WriterTask["Writer&lt;E&gt;<br/><i>byte pump</i>"]
        EventFwd["Event Forwarder<br/><i>broadcast relay</i>"]
    end

    subgraph "rayon ThreadPool (blocking)"
        DLLoop["Backend::run_downloader<br/><i>orchestration loop</i>"]
        DecodeWorker["AudioWorker<br/><i>decode + effects</i>"]
    end

    subgraph "Shared State (lock-based)"
        StorageRes["StorageResource<br/><i>Mutex + Condvar</i>"]
        SegIdx["SegmentIndex / SharedSegments<br/><i>Mutex + Condvar</i>"]
        Progress["Progress<br/><i>AtomicU64 + Notify</i>"]
    end

    subgraph "Channels"
        PcmChan["kanal::bounded&lt;PcmChunk&gt;<br/><i>decode -> consumer</i>"]
        CmdChan["kanal::bounded&lt;AudioCommand&gt;<br/><i>consumer -> worker</i>"]
        EventChan["broadcast::channel&lt;Event&gt;<br/><i>all -> consumer</i>"]
    end

    DLLoop -- "plan + fetch" --> NetTask
    NetTask -- "bytes" --> WriterTask
    WriterTask -- "write_at()" --> StorageRes
    DLLoop -- "commit()" --> SegIdx

    DecodeWorker -- "wait_range() blocks" --> StorageRes
    DecodeWorker -- "read_at()" --> StorageRes
    DecodeWorker -- "PcmChunk" --> PcmChan
    Resource -- "recv()" --> PcmChan

    Resource -- "Seek cmd" --> CmdChan
    CmdChan --> DecodeWorker

    EventFwd -- "ResourceEvent" --> EventChan
    EventChan --> App

    DLLoop -- "should_throttle()" --> Progress
    DecodeWorker -- "set_read_pos()" --> Progress

    style StorageRes fill:#d4a574,color:#000
    style SegIdx fill:#d4a574,color:#000
    style Progress fill:#d4a574,color:#000
    style PcmChan fill:#5b8a5b,color:#fff
    style CmdChan fill:#5b8a5b,color:#fff
    style EventChan fill:#5b8a5b,color:#fff
```

---

### Async-to-Sync Bridge (Storage)

Detailed view of the central synchronization mechanism:

```mermaid
sequenceDiagram
    participant DL as Downloader (async)
    participant W as Writer
    participant SR as StorageResource
    participant RS as RangeSet
    participant CV as Condvar
    participant R as Reader (sync)

    Note over DL,R: Async downloader and sync reader share StorageResource

    DL->>W: poll next chunk from network
    W->>SR: write_at(offset, bytes)
    SR->>RS: insert(offset..offset+len)
    SR->>CV: notify_all()

    R->>SR: wait_range(pos..pos+len)
    SR->>RS: check coverage
    alt Range not ready
        SR->>CV: wait(50ms timeout)
        Note over SR,CV: Loop until ready, EOF, or cancelled
        CV-->>SR: woken by notify_all
        SR->>RS: re-check coverage
    end
    SR-->>R: WaitOutcome::Ready

    R->>SR: read_at(pos, buf)
    SR-->>R: bytes read

    Note over DL,R: Backpressure via Progress atomics
    DL->>DL: should_throttle()? (download_pos - read_pos > limit)
    R->>R: set_read_pos(pos)
    R-->>DL: Notify (reader advanced)
```

---

### Assets Decorator Chain

Request flow through the `AssetStore` decorator stack:

```mermaid
sequenceDiagram
    participant Caller
    participant Cache as CachedAssets
    participant Lease as LeaseAssets
    participant Proc as ProcessingAssets
    participant Evict as EvictAssets
    participant Disk as DiskAssetStore
    participant FS as Filesystem

    Caller->>Cache: open_resource_with_ctx(key, ctx)
    Cache->>Cache: check LRU cache
    alt Cache hit
        Cache-->>Caller: cached resource
    else Cache miss
        Cache->>Lease: open_resource_with_ctx(key, ctx)
        Lease->>Lease: pin(asset_root)
        Lease->>Lease: persist _index/pins.bin
        Lease->>Proc: open_resource_with_ctx(key, ctx)
        Proc->>Evict: open_resource_with_ctx(key, ctx)
        Evict->>Evict: first access? check LRU limits
        alt Over limits
            Evict->>Evict: evict oldest unpinned assets
        end
        Evict->>Disk: open_resource_with_ctx(key, ctx)
        Disk->>FS: MmapResource::open(path, mode)
        FS-->>Disk: StorageResource::Mmap
        Disk-->>Evict: StorageResource
        Evict-->>Proc: StorageResource
        Proc->>Proc: wrap in ProcessedResource(res, ctx)
        Proc-->>Lease: ProcessedResource
        Lease->>Lease: wrap in LeaseResource(res, guard)
        Lease-->>Cache: LeaseResource
        Cache->>Cache: insert into LRU cache
        Cache-->>Caller: LeaseResource
    end

    Note over Caller: On commit():
    Caller->>Proc: commit(final_len)
    alt ctx is Some (encrypted)
        Proc->>Proc: read 64KB chunks from disk
        Proc->>Proc: aes128_cbc_process_chunk()
        Proc->>Proc: write decrypted back to disk
    end
    Proc->>Disk: commit(actual_len)

    Note over Caller: On drop(LeaseResource):
    Caller->>Lease: drop LeaseGuard
    Lease->>Lease: unpin(asset_root)
    Lease->>Lease: persist _index/pins.bin
```

---

### Net Decorator Chain

HTTP request flow through composed decorators:

```mermaid
sequenceDiagram
    participant Caller
    participant Retry as RetryNet
    participant Timeout as TimeoutNet
    participant HTTP as HttpClient
    participant Server as HTTP Server

    Caller->>Retry: get_bytes(url, headers)
    loop attempt 0..max_retries
        Retry->>Timeout: get_bytes(url, headers)
        Timeout->>Timeout: start tokio::time::timeout
        Timeout->>HTTP: get_bytes(url, headers)
        HTTP->>Server: GET url
        alt Success (2xx)
            Server-->>HTTP: 200 OK + body
            HTTP-->>Timeout: Ok(Bytes)
            Timeout-->>Retry: Ok(Bytes)
            Retry-->>Caller: Ok(Bytes)
        else Timeout elapsed
            Timeout-->>Retry: Err(NetError::Timeout)
            Retry->>Retry: is_retryable? Yes
            Retry->>Retry: sleep(base_delay * 2^attempt)
            Note over Retry: tokio::select with CancellationToken
        else Server error (5xx)
            Server-->>HTTP: 503
            HTTP-->>Timeout: Err(HttpError{503})
            Timeout-->>Retry: Err(HttpError{503})
            Retry->>Retry: is_retryable? Yes
            Retry->>Retry: sleep(backoff)
        else Client error (4xx)
            Server-->>HTTP: 404
            HTTP-->>Timeout: Err(HttpError{404})
            Timeout-->>Retry: Err(HttpError{404})
            Retry->>Retry: is_retryable? No
            Retry-->>Caller: Err(HttpError{404})
        end
    end
    Retry-->>Caller: Err(RetryExhausted)
```

---

### ABR Decision Flow

Adaptive bitrate decision-making process:

```mermaid
graph TD
    Start["AbrController::decide(now)"]
    Manual{"AbrMode::Manual?"}
    Interval{"now - last_switch<br/>< min_switch_interval?"}
    Estimate{"estimator.estimate_bps()<br/>returns Some?"}
    Select["Select best variant<br/>bandwidth <= estimate / safety_factor"]
    CompareUp{"candidate_bw ><br/>current_bw?"}
    CompareDown{"candidate_bw <<br/>current_bw?"}
    BufferCheck{"buffer_level >=<br/>min_buffer_for_up_switch?"}
    HystUp{"throughput >=<br/>candidate * up_hysteresis?"}
    HystDown{"buffer <= down_switch_buffer<br/>OR throughput <=<br/>current * down_hysteresis?"}

    RetManual["ManualOverride<br/>(fixed variant)"]
    RetMinInt["MinInterval<br/>(too soon)"]
    RetNoEst["NoEstimate<br/>(keep current)"]
    RetBufLow["BufferTooLowForUpSwitch<br/>(keep current)"]
    RetOptimal["AlreadyOptimal<br/>(keep current)"]
    RetUp["UpSwitch<br/>(upgrade quality)"]
    RetDown["DownSwitch<br/>(downgrade quality)"]
    RetKeep["AlreadyOptimal<br/>(keep current)"]

    Start --> Manual
    Manual -- "Yes" --> RetManual
    Manual -- "No" --> Interval
    Interval -- "Yes" --> RetMinInt
    Interval -- "No" --> Estimate
    Estimate -- "None" --> RetNoEst
    Estimate -- "Some(bps)" --> Select
    Select --> CompareUp
    CompareUp -- "Yes" --> BufferCheck
    CompareUp -- "No" --> CompareDown
    BufferCheck -- "No" --> RetBufLow
    BufferCheck -- "Yes" --> HystUp
    HystUp -- "No" --> RetOptimal
    HystUp -- "Yes" --> RetUp
    CompareDown -- "Yes" --> HystDown
    CompareDown -- "No (equal)" --> RetKeep
    HystDown -- "Yes" --> RetDown
    HystDown -- "No" --> RetKeep

    style RetUp fill:#5b8a5b,color:#fff
    style RetDown fill:#a05050,color:#fff
    style RetManual fill:#6b6b8b,color:#fff
    style RetMinInt fill:#8b8b6b,color:#000
    style RetNoEst fill:#8b8b6b,color:#000
    style RetBufLow fill:#8b8b6b,color:#000
    style RetOptimal fill:#5577aa,color:#fff
    style RetKeep fill:#5577aa,color:#fff
```

---

### Event Propagation

How events flow from protocol crates to the consumer:

```mermaid
graph LR
    subgraph "Protocol Layer"
        FE["FileEvent<br/><i>DownloadProgress<br/>DownloadComplete<br/>PlaybackProgress</i>"]
        HE["HlsEvent<br/><i>VariantsDiscovered<br/>VariantApplied<br/>SegmentComplete<br/>ThroughputSample</i>"]
    end

    subgraph "Audio Layer"
        AE["AudioEvent<br/><i>FormatDetected<br/>FormatChanged<br/>SeekComplete<br/>EndOfStream</i>"]
    end

    subgraph "Pipeline Layer"
        APE_F["AudioPipelineEvent&lt;FileEvent&gt;<br/><i>Stream(FileEvent)<br/>Audio(AudioEvent)</i>"]
        APE_H["AudioPipelineEvent&lt;HlsEvent&gt;<br/><i>Stream(HlsEvent)<br/>Audio(AudioEvent)</i>"]
    end

    subgraph "Facade Layer"
        RE["ResourceEvent<br/><i>unified enum</i>"]
    end

    subgraph "Consumer"
        App["Application<br/><i>broadcast::Receiver</i>"]
    end

    FE -- "broadcast" --> APE_F
    AE -- "broadcast" --> APE_F
    AE -- "broadcast" --> APE_H
    HE -- "broadcast" --> APE_H
    APE_F -- "tokio task<br/>From impl" --> RE
    APE_H -- "tokio task<br/>From impl" --> RE
    RE -- "broadcast" --> App

    style RE fill:#4a6fa5,color:#fff
```

---

### Buffer Pool Lifecycle

Allocation and recycling flow in `kithara-bufpool`:

```mermaid
sequenceDiagram
    participant Caller
    participant Pool as SharedPool<32, Vec<u8>>
    participant Home as Shard[home] (Mutex)
    participant Other as Shard[other] (Mutex)
    participant Heap as Allocator

    Caller->>Pool: get_with(init_fn)
    Pool->>Pool: shard_idx = hash(thread_id) % 32
    Pool->>Home: lock & pop()
    alt Home shard has buffer
        Home-->>Pool: Some(buf)
    else Home shard empty
        Pool->>Other: lock & pop() (work-stealing, try all shards)
        alt Other shard has buffer
            Other-->>Pool: Some(buf)
        else All shards empty
            Pool->>Heap: Vec::default()
            Heap-->>Pool: new Vec
        end
    end
    Pool->>Pool: init_fn(&mut buf)
    Pool-->>Caller: Pooled { buf, pool, shard_idx }

    Note over Caller: Use buffer...

    Caller->>Caller: drop(Pooled)
    Caller->>Pool: put(buf, shard_idx)
    Pool->>Pool: buf.reuse(trim_capacity)
    alt Shard not full & reuse=true
        Pool->>Home: lock & push(buf)
        Note over Home: Buffer recycled
    else Shard full or capacity=0
        Pool->>Heap: drop(buf)
        Note over Heap: Buffer deallocated
    end
```

---

### HLS Segment Stitching

Virtual stream layout and mid-stream ABR variant switch:

```mermaid
graph LR
    subgraph "Single Variant Playback"
        direction LR
        I0["Init V0"]
        S0["Seg 0"]
        S1["Seg 1"]
        S2["Seg 2"]
        S3["Seg 3"]
        I0 --- S0 --- S1 --- S2 --- S3
    end

    subgraph "Mid-Stream ABR Switch (V0 -> V1)"
        direction LR
        A0["V0 Init"]
        A1["V0 Seg0"]
        A2["V0 Seg1"]
        FENCE["fence_at()"]
        B0["V1 Init"]
        B1["V1 Seg2"]
        B2["V1 Seg3"]
        A0 --- A1 --- A2 -.- FENCE
        FENCE -.- B0 --- B1 --- B2
    end

    style FENCE fill:#a05050,color:#fff
    style I0 fill:#6b8cae,color:#fff
    style A0 fill:#6b8cae,color:#fff
    style B0 fill:#5b8a5b,color:#fff
```

---

### File Three-Phase Download

State machine for progressive file download phases:

```mermaid
stateDiagram-v2
    [*] --> Sequential: File::create()

    Sequential --> GapFilling: Stream ends early\nor network error
    Sequential --> Complete: All bytes received\ncommit(total_len)

    GapFilling --> GapFilling: Range request\ncompletes a gap
    GapFilling --> Complete: All gaps filled\ncoverage.is_complete()

    Complete --> [*]

    state Sequential {
        [*] --> Streaming
        Streaming: Writer pumps bytes\nfrom HTTP stream to StorageResource
        Streaming --> Throttled: download_pos - read_pos > look_ahead_bytes
        Throttled --> Streaming: Reader advances\nProgress::reader_advanced
        Streaming --> StreamEnded: HTTP stream EOF
    }

    state GapFilling {
        [*] --> FindGaps
        FindGaps: coverage.next_gap(2MB)
        FindGaps --> BatchFetch: Up to 4 gaps
        BatchFetch: Parallel HTTP Range requests
        BatchFetch --> CommitGap: Range downloaded
        CommitGap: Mark coverage,\nflush to disk
        CommitGap --> FindGaps: More gaps?
    }
```

---

### Seek Sequence (File)

What happens when the consumer seeks during progressive download:

```mermaid
sequenceDiagram
    participant App as Consumer
    participant Audio as Audio<Stream<File>>
    participant Worker as AudioWorker
    participant Decoder as Symphonia
    participant Stream as Stream<File>
    participant Source as FileSource
    participant Shared as SharedFileState
    participant DL as FileDownloader
    participant Net as HttpClient
    participant SR as StorageResource

    App->>Audio: seek(Duration)
    Audio->>Audio: epoch += 1
    Audio->>Worker: AudioCommand::Seek { position, epoch }

    Worker->>Decoder: seek(position)
    Decoder->>Stream: seek(SeekFrom::Start(byte_pos))
    Stream->>Source: (updates reader pos)

    Note over Source,DL: Next read triggers on-demand download

    Worker->>Decoder: next_chunk()
    Decoder->>Stream: read(buf)
    Stream->>Source: wait_range(pos..pos+len)
    Source->>Source: range beyond download_pos?

    alt Data already available
        Source->>SR: wait_range() -> Ready
        SR-->>Source: data
    else Data not downloaded yet
        Source->>Shared: request_range(pos..pos+len)
        Shared->>Shared: SegQueue.push(range)
        Shared->>DL: reader_needs_data.notify()
        DL->>DL: poll_demand() -> Some(range)
        DL->>Net: get_range(url, range)
        Net-->>DL: byte stream
        DL->>SR: write_at(pos, bytes)
        SR->>SR: notify_all() (Condvar)
        Source->>SR: wait_range() -> Ready
        SR-->>Source: data
    end

    Source-->>Stream: bytes
    Stream-->>Decoder: bytes
    Decoder-->>Worker: PcmChunk { epoch: new }
    Worker-->>Audio: PcmChunk via kanal
    Audio->>Audio: epoch matches -> accept
    Audio-->>App: samples
```

---

### HLS ABR Variant Switch

Sequence when ABR decides to switch quality mid-stream:

```mermaid
sequenceDiagram
    participant ABR as AbrController
    participant DL as HlsDownloader
    participant FM as FetchManager
    participant Net as HttpClient
    participant SI as SegmentIndex
    participant Src as HlsSource
    participant Dec as Decoder
    participant Audio as Audio<Stream<Hls>>

    Note over DL: plan() called for next batch

    DL->>ABR: decide(now)
    ABR->>ABR: estimate_bps() via dual EWMA
    ABR-->>DL: AbrDecision { target: V1, changed: true, reason: UpSwitch }

    DL->>DL: emit VariantApplied { from: V0, to: V1 }
    DL->>DL: ensure_variant_ready(V1, seg_idx)

    DL->>FM: HEAD requests for V1 segments
    FM->>Net: head(seg_url) [parallel for all segments]
    Net-->>FM: Content-Length per segment
    FM-->>DL: segment metadata + total length

    DL->>SI: fence_at(current_offset, keep_variant=V1)
    Note over SI: Discard V0 segments after fence

    DL->>FM: load_segment(V1, init + first_segment)
    FM->>Net: GET init segment
    Net-->>FM: init bytes
    FM->>Net: GET media segment
    Net-->>FM: segment bytes

    DL->>SI: push(SegmentEntry { V1, byte_offset, init_len, meta })
    DL->>SI: condvar.notify_all()

    Note over Src,Dec: Decoder reads through the switch

    Src->>SI: wait_range() -> Ready (new segment)
    Src->>Src: detect variant change via media_info()
    Src-->>Dec: format_change_segment_range()

    Dec->>Dec: poll_format_change() -> Some(new MediaInfo)
    Dec->>Dec: recreate decoder for new codec/container
    Dec->>Src: clear_variant_fence()
    Dec->>Src: read_at(new_segment_offset) -> init + media data

    Dec-->>Audio: PcmChunk (new variant, seamless)
```

---

## kithara (facade)

**Path:** `crates/kithara`

The top-level crate that provides a unified, type-erased API for audio playback from any
supported source. Consumers interact primarily with `Resource`.

### Key Types

| Type | Role |
|------|------|
| `Resource` | Type-erased wrapper over `Box<dyn PcmReader>`. Single entry point for reading PCM. |
| `ResourceConfig` | Builder for creating a `Resource`. Holds URL/path, network options, ABR options, cache options, etc. |
| `ResourceSrc` | Enum: `Url(Url)` or `Path(PathBuf)`. |
| `SourceType` | Auto-detection enum: `RemoteFile`, `LocalFile`, `HlsStream`. Determined by URL extension (`.m3u8` -> HLS). |
| `ResourceEvent` | Unified event enum aggregating all upstream events (audio, file, HLS, ABR). |

### How It Works

1. `ResourceConfig::new(input)` parses a URL or absolute path into `ResourceSrc`.
2. `Resource::new(config)` calls `SourceType::detect()` to classify the source.
3. Based on source type, constructs either `Audio<Stream<File>>` or `Audio<Stream<Hls>>`.
4. The resulting audio pipeline is type-erased into `Box<dyn PcmReader>`.
5. A background tokio task forwards typed events into a unified `ResourceEvent` broadcast.
6. Consumer reads PCM via `resource.read(&mut buf)` or `resource.read_planar(...)`.

### Feature Flags

| Feature | Effect |
|---------|--------|
| `file` (default) | Enables progressive file download |
| `hls` (default) | Enables HLS streaming + ABR |
| `rodio` | Implements `rodio::Source` on `Resource` |
| `perf` | Propagates performance instrumentation to all crates |
| `full` | Enables all features |

### Re-exports

The crate re-exports all sub-crates as public modules (`kithara::audio`, `kithara::decode`,
`kithara::stream`, `kithara::file`, `kithara::hls`, `kithara::abr`, `kithara::net`, etc.)
and provides a `prelude` module with the most commonly used types.

---

## kithara-bufpool

**Path:** `crates/kithara-bufpool`

Generic, thread-safe, sharded buffer pool for zero-allocation hot paths.

### Core Design

The pool is parameterized at compile time by the number of shards (`SHARDS`) and the buffer
type (`T: Reuse`). Each shard is a `parking_lot::Mutex<Vec<T>>`. Thread affinity is achieved
by hashing the current thread ID to select a "home" shard. When the home shard is empty, the
pool work-steals from other shards before falling back to `T::default()`.

### Key Types

| Type | Description |
|------|-------------|
| `Reuse` (trait) | Defines how a buffer resets itself. `Vec<T>` has a built-in implementation that clears and optionally shrinks. |
| `Pool<SHARDS, T>` | The core sharded pool. |
| `Pooled<'a, SHARDS, T>` | RAII borrowed wrapper. Automatically returns the buffer to the pool on drop. Implements `Deref`/`DerefMut`. |
| `PooledOwned<SHARDS, T>` | RAII owned wrapper (holds `Arc<Pool>`). `'static`, suitable for async contexts. |
| `SharedPool<SHARDS, T>` | Convenience wrapper around `Arc<Pool>` with `get()`, `recycle()`, `attach()`. |
| `PooledSlice` / `PooledSliceOwned` | Wrappers with tracked length for partial-fill scenarios (e.g., network reads). |

### Global Pools

Two pre-configured global pools are exposed via `global.rs`:

```rust
pub type BytePool = SharedPool<32, Vec<u8>>;   // 1024 max buffers, 64 KB trim
pub type PcmPool  = SharedPool<32, Vec<f32>>;  // 64 max buffers, 200K trim

pub fn byte_pool() -> &'static BytePool;
pub fn pcm_pool()  -> &'static PcmPool;
```

A `global_pool!` macro allows defining additional global pools.

### Allocation Flow

1. **Get:** Lock home shard -> pop. If empty, try other shards (work-stealing). If all empty,
   allocate via `T::default()`. Apply initialization closure.
2. **Return (drop):** Call `value.reuse(trim_capacity)` to clear and shrink. If shard is not
   full and reuse returns `true`, push back. Otherwise, drop silently.

### Feature Flags

- `perf`: Enables `hotpath::measure` on the `get_with` method for profiling.

---

## kithara-storage

**Path:** `crates/kithara-storage`

Unified, backend-agnostic storage layer with two drivers: mmap (filesystem) and in-memory.
Designed for streaming I/O with incremental writes, range tracking, and atomic operations.

### Key Types

| Type | Description |
|------|-------------|
| `ResourceExt` (trait) | Consumer-facing API: `read_at`, `write_at`, `wait_range`, `commit`, `fail`, `reactivate`, `read_into`, `write_all`. |
| `Driver` (trait) | Backend abstraction with associated `Options` type. |
| `StorageResource` | Enum dispatching to `MmapResource` or `MemResource`. |
| `OpenMode` | `Auto` (default), `ReadWrite`, `ReadOnly`. |
| `ResourceStatus` | `Active`, `Committed { final_len }`, `Failed(String)`. |
| `WaitOutcome` | `Ready` or `Eof`. |
| `Atomic<R>` | Decorator for crash-safe writes via write-temp-rename. |
| `Coverage` (trait) | Tracks downloaded byte ranges. `MemCoverage` is the in-memory impl. |

### Mmap vs. Mem Drivers

| Aspect | MmapDriver | MemDriver |
|--------|-----------|-----------|
| Backing | `mmap-io::MemoryMappedFile` | `Vec<u8>` behind `Mutex` |
| Lock-free fast path | Yes (`SegQueue` for write notifications) | No |
| Auto-resize | 2x growth on overflow | Extend on write |
| Post-commit writes | Only in `ReadWrite` mode | Never |
| `path()` | `Some` | `None` |

### Range Tracking

Internally uses `RangeSet<u64>` (from `rangemap`) to track which byte ranges have been
written. `wait_range` blocks (via `Condvar` with 50 ms timeout) until the requested range is
fully covered, or returns `Eof` when the resource is committed and the range starts beyond
the final length.

### Synchronization

- `parking_lot::Mutex<CommonState>` + `Condvar` for state and coordination.
- `crossbeam_queue::SegQueue<Range<u64>>` for lock-free fast-path on mmap writes.
- `CancellationToken` checked at operation entry and during wait loops.

---

## kithara-assets

**Path:** `crates/kithara-assets`

Disk and in-memory asset store with decorator-based feature composition: caching, leasing
(pinning), processing (decryption), and LRU eviction.

### Decorator Chain

The full stack (outermost to innermost):

```
CachedAssets<LeaseAssets<ProcessingAssets<EvictAssets<DiskAssetStore>>>>
```

Each layer wraps the previous and implements the `Assets` trait. Layers can be
independently enabled/disabled via builder.

| Layer | Responsibility |
|-------|---------------|
| `CachedAssets` | In-memory LRU cache (default 5 entries). Prevents duplicate mmap opens. |
| `LeaseAssets` | RAII-based pinning. Every `open_resource()` pins the asset root; `LeaseGuard` unpins on drop. Prevents eviction of in-use assets. |
| `ProcessingAssets` | Optional chunk-based transformation (e.g., AES-128-CBC decryption) on `commit()`. Uses 64 KB chunks from buffer pool. |
| `EvictAssets` | LRU eviction by asset count and/or byte size. Pinned assets excluded. Soft caps (best-effort). |
| `DiskAssetStore` | Base disk I/O. Maps `ResourceKey` to filesystem paths under `<root_dir>/<asset_root>/`. |
| `MemAssetStore` | In-memory variant using `DashMap<ResourceKey, MemResource>`. |

### Key Types

| Type | Description |
|------|-------------|
| `Assets` (trait) | Base abstraction with associated `Res`, `Context`, `IndexRes` types. |
| `ResourceKey` | `Relative(String)` or `Absolute(PathBuf)`. |
| `AssetStore<Ctx>` | Full disk decorator stack (type alias). |
| `MemStore<Ctx>` | Full in-memory decorator stack (type alias). |
| `AssetsBackend<Ctx>` | Enum: `Disk(AssetStore)` or `Mem(MemStore)`. |
| `AssetStoreBuilder<Ctx>` | Fluent builder for constructing the full stack. |
| `AssetResource<Ctx>` | `LeaseResource<ProcessedResource<StorageResource, Ctx>, LeaseGuard>`. |

### Index Persistence

Three index types are persisted under `_index/` for crash recovery:

| Index | File | Purpose |
|-------|------|---------|
| `PinsIndex` | `_index/pins.bin` | Persists pinned asset roots. |
| `LruIndex` + `LruState` | `_index/lru.bin` | Monotonic clock + byte accounting for eviction decisions. |
| `CoverageIndex` + `DiskCoverage` | `_index/cov.bin` | Per-segment byte-range coverage for partial download tracking. |

All indices use bincode + `Atomic<R>` for crash-safe writes.

### Asset Root Generation

Asset roots are derived from URLs via `SHA256(canonical_url + optional_name)`, producing a
32-character hex string. URL canonicalization removes query/fragment, normalizes scheme/host,
and strips default ports.

---

## kithara-net

**Path:** `crates/kithara-net`

Async HTTP networking built on `reqwest` with trait-based abstractions and composable
decorators.

### Core Trait

```rust
#[async_trait]
pub trait Net: Send + Sync {
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;
    async fn get_range(&self, url: Url, range: RangeSpec, headers: Option<Headers>)
        -> Result<ByteStream, NetError>;
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError>;
}
```

### Decorators

| Decorator | Behavior |
|-----------|----------|
| `TimeoutNet<N>` | Wraps all methods with `tokio::time::timeout`. |
| `RetryNet<N, P>` | Exponential backoff retry. Classifies errors as retryable (5xx, 429, 408, timeouts) or not (4xx except 408/429). Respects `CancellationToken`. |

Decorators compose via `NetExt` extension trait:
```rust
HttpClient::new(opts).with_retry(policy, cancel).with_timeout(duration)
```

### Key Types

| Type | Description |
|------|-------------|
| `HttpClient` | `reqwest::Client` wrapper implementing `Net`. |
| `Headers` | `HashMap<String, String>` wrapper. |
| `RangeSpec` | HTTP byte range: `{ start: u64, end: Option<u64> }`. |
| `RetryPolicy` | `{ base_delay, max_delay, max_retries }`. Default: 3 retries, 100 ms base, 5 s max. |
| `NetOptions` | `{ pool_max_idle_per_host, request_timeout, retry_policy }`. |
| `NetError` | `Http`, `InvalidRange`, `Timeout`, `RetryExhausted`, `HttpError { status, url, body }`, `Cancelled`. |

### Timeout Behavior

- `get_bytes()`, `head()`: apply `request_timeout` from options.
- `stream()`: **no timeout** (designed for long downloads).
- Decorator layer can override with custom timeout.

---

## kithara-stream

**Path:** `crates/kithara-stream`

Core byte-stream orchestration that bridges async downloading with sync `Read + Seek`.
This is the central abstraction enabling progressive playback.

### Key Traits

| Trait | Purpose |
|-------|---------|
| `Source` | Sync random-access interface: `wait_range`, `read_at`, `len`, `media_info`, segment tracking. |
| `Downloader` | Async planner: `plan()` returns batches or step mode; `commit()` stores results; `should_throttle()` / `wait_ready()` for backpressure. |
| `DownloaderIo` | Pure I/O (network fetch), `Clone + Send`, stateless. Runs multiple copies in parallel. |
| `StreamType` | Marker trait for protocol types (`File`, `Hls`). Associated types: `Config`, `Source`, `Error`, `Event`. |

### Key Structs

| Struct | Description |
|--------|-------------|
| `Reader<S: Source>` | Sync `Read + Seek` wrapper. Calls `source.wait_range()` then `source.read_at()`. |
| `Writer<E>` | Async byte pump. Implements `futures::Stream`, writes chunks to `StorageResource` via `write_at`. |
| `Backend` | Spawns downloader task on thread pool. Orchestration loop: drain demand -> plan -> fetch in parallel -> commit. |
| `Stream<T: StreamType>` | Generic stream wrapping `Reader<T::Source>`. Implements `Read + Seek`. Supports format change signaling for ABR. |
| `ThreadPool` | Wrapper around `rayon::ThreadPool` for blocking work. |

### Canonical Types

Defined here as the single source of truth:

```rust
pub enum AudioCodec { AacLc, AacHe, AacHeV2, Mp3, Flac, Vorbis, Opus, Alac, Pcm, Adpcm }
pub enum ContainerFormat { Fmp4, MpegTs, MpegAudio, Adts, Flac, Wav, Ogg, Caf, Mkv }
pub struct MediaInfo { channels, codec, container, sample_rate, variant_index }
```

### Async-to-Sync Bridge

The fundamental design pattern:

1. **Downloader** (async) writes data to `StorageResource` via `Writer`.
2. **Reader** (sync) calls `StorageResource::wait_range()`, which blocks until bytes are
   available.
3. `StorageResource` is the synchronization point -- no channels between reader and
   downloader.
4. **Backpressure**: downloader pauses via `should_throttle()` when too far ahead of the
   reader.
5. **On-demand**: reader can request specific ranges (for seeks) that bypass backpressure.

### Epoch-Based Stale Filtering

`EpochValidator` increments epoch on seek. Chunks from previous epochs are discarded,
preventing old data from reaching the decoder after a seek.

---

## kithara-file

**Path:** `crates/kithara-file`

Progressive HTTP file download. Implements `StreamType` for use as `Stream<File>`.

### Key Types

| Type | Description |
|------|-------------|
| `File` | Marker type implementing `StreamType`. |
| `FileConfig` | Source (`FileSrc::Remote(Url)` or `FileSrc::Local(PathBuf)`), network options, store options, backpressure (`look_ahead_bytes`). |
| `FileSource` | Implements `Source`. Wraps `AssetResource` with progress tracking. |
| `FileEvent` | `DownloadProgress`, `DownloadComplete`, `DownloadError`, `PlaybackProgress`, `Error`, `EndOfStream`. |

### Three-Phase Download

| Phase | Strategy |
|-------|----------|
| Sequential | Stream from file start via `Writer`. Fast path for complete downloads. |
| Gap Filling | HTTP Range requests for missing chunks. Batches up to 4 gaps, each up to 2 MB. |
| Complete | All data downloaded; no further activity. |

### Local File Handling

When `FileSrc::Local(path)`, the crate opens the file via `AssetStore` with an absolute
`ResourceKey`, skips all network activity, and creates a fully-cached `FileSource` with no
background downloader.

### On-Demand Downloads

`SharedFileState` carries range requests between the sync reader and async downloader via a
lock-free `SegQueue`. When the reader seeks beyond the current download position, it pushes a
range request that the downloader picks up with higher priority than sequential downloading.

### Coverage Tracking

`FileCoverage` wraps either `MemCoverage` (ephemeral) or `DiskCoverage` (persistent). The
disk variant persists to `_index/cov.bin` for crash-safe recovery of partial downloads.

---

## kithara-hls

**Path:** `crates/kithara-hls`

HLS VOD orchestration with ABR, caching, and optional encryption support. Implements
`StreamType` for use as `Stream<Hls>`.

### Key Types

| Type | Description |
|------|-------------|
| `Hls` | Marker type implementing `StreamType`. |
| `HlsConfig` | Master playlist URL, ABR options, key options, network/store options, batch size, backpressure. |
| `HlsSource` | Implements `Source`. Provides sync random-access reading from stitched segments. |
| `FetchManager` | Unified fetch layer: network + `AssetsBackend` for disk or in-memory cache. Parses and caches playlists. |
| `HlsDownloader` | `Downloader` implementation. Makes ABR decisions, batches segment downloads, handles variant switches. |
| `HlsEvent` | `VariantsDiscovered`, `VariantApplied`, `SegmentStart`, `SegmentProgress`, `SegmentComplete`, `ThroughputSample`, download/playback events. |

### Playlist Parsing

Located in `parsing.rs`. Uses the `hls_m3u8` crate.

- `parse_master_playlist(data)` -> `MasterPlaylist { variants: Vec<VariantStream> }`.
  Extracts bandwidth, codecs, container format from `STREAM-INF` tags.
- `parse_media_playlist(data, variant_id)` -> `MediaPlaylist { segments, init_segment, current_key, ... }`.
  Parses segments with durations, tracks encryption keys via `#EXT-X-KEY`, detects init
  segments via `#EXT-X-MAP`.

### Virtual Stream Layout

HLS segments are logically stitched into a contiguous byte stream:

```
Single variant:
  [Init] [Seg0] [Seg1] [Seg2] ...
  0──────┬──────┬──────┬──────┤

Mid-stream variant switch (ABR):
  [V0 Seg0] [V0 Seg1] (fence) [V3 Init] [V3 Seg14] [V3 Seg15] ...
  0─────────┬─────────┬────────┬─────────┬──────────┤
```

`SegmentIndex` maintains a `HashMap<(variant, segment_index), SegmentEntry>` plus a
`RangeSet<u64>` for loaded byte ranges. `fence_at()` discards old variant segments on ABR
switch.

### Shared State

`SharedSegments` coordinates the async downloader and sync source:
- `segments: Mutex<SegmentIndex>` + `condvar: Condvar`
- `eof: AtomicBool`, `reader_offset: AtomicU64`
- `segment_requests: SegQueue<SegmentRequest>` for on-demand loading

### ABR Integration

1. `plan()` calls `make_abr_decision()` which queries `AbrController`.
2. On variant change: emit `VariantApplied`, calculate new variant metadata via HEAD requests.
3. `ensure_variant_ready()` handles mid-stream switches by fencing old segments.
4. Throughput samples are fed to ABR after each segment download.

### Caching and Offline Support

- Leverages `AssetsBackend<DecryptContext>` (disk or ephemeral).
- `populate_cached_segments()` scans disk for committed segments on startup.
- `allow_cache` flag from `#EXT-X-ALLOW-CACHE` respected.
- `CoverageIndex` tracks per-segment coverage for crash recovery.

---

## kithara-abr

**Path:** `crates/kithara-abr`

Protocol-agnostic adaptive bitrate algorithm.

### Core: AbrController

```rust
pub struct AbrController<E: Estimator> { ... }
```

Operates in two modes:
- **Auto**: Adaptive quality switching based on throughput estimation and buffer level.
- **Manual**: Fixed variant, ignores network conditions.

### Decision Logic

1. If in Manual mode -> return current variant with `ManualOverride` reason.
2. If less than `min_switch_interval` (30 s) since last switch -> return current
   (`MinInterval`).
3. Estimate throughput via dual-track EWMA.
4. Select highest-bandwidth variant not exceeding `estimate / safety_factor`.
5. **Up-switch**: requires buffer >= `min_buffer_for_up_switch_secs` (10 s) AND throughput
   >= `candidate_bw * up_hysteresis_ratio` (1.3x).
6. **Down-switch**: triggered by buffer <= `down_switch_buffer_secs` (5 s) OR throughput
   <= `current_bw * down_hysteresis_ratio` (0.8x).

### Throughput Estimation

`ThroughputEstimator` uses dual-track EWMA:
- **Fast** (2 s half-life): responds quickly to changes.
- **Slow** (10 s half-life): provides stability.
- Final estimate = `min(fast, slow)` -- conservative.

Samples below 16,000 bytes are filtered as noise. Cache hits set `initial_bps = 100 Mbps`
to allow immediate quality upgrades.

### Key Types

| Type | Description |
|------|-------------|
| `AbrOptions` | Full configuration: hysteresis ratios, buffer thresholds, sample window, mode, variants. |
| `AbrDecision` | `{ target_variant_index, reason, changed }`. |
| `AbrReason` | `Initial`, `ManualOverride`, `UpSwitch`, `DownSwitch`, `MinInterval`, `NoEstimate`, `BufferTooLowForUpSwitch`, `AlreadyOptimal`. |
| `Variant` | `{ variant_index, bandwidth_bps }`. |
| `VariantInfo` | Extended metadata for UI: index, bandwidth, name, codecs, container. |
| `ThroughputSample` | `{ bytes, duration, at, source, content_duration }`. |
| `Estimator` (trait) | Pluggable estimation strategy. |

---

## kithara-drm

**Path:** `crates/kithara-drm`

AES-128-CBC segment decryption for encrypted HLS streams.

### Public API

```rust
pub struct DecryptContext {
    pub key: [u8; 16],
    pub iv: [u8; 16],
}

pub fn aes128_cbc_process_chunk(
    input: &[u8], output: &mut [u8], ctx: &DecryptContext, is_last: bool,
) -> Result<usize, String>
```

### Integration

The function signature matches `ProcessChunkFn<DecryptContext>` from `kithara-assets`.
It is set as the processing callback when building `AssetsBackend<DecryptContext>` in the
HLS crate.

- **Intermediate chunks** (`is_last = false`): block-by-block decryption, output = input
  size.
- **Final chunk** (`is_last = true`): PKCS7 padding removal, output <= input size.
- Input must be aligned to 16-byte AES block size.
- All operations are in-place (no buffer allocation).

### Key Derivation

IV derivation happens in `kithara-hls::keys::KeyManager`:
- If the `#EXT-X-KEY` tag provides an explicit IV, it is used directly.
- Otherwise, IV is derived from segment sequence number:
  `[0u8; 8] || sequence.to_be_bytes()`.

---

## kithara-decode

**Path:** `crates/kithara-decode`

Audio decoding with multiple backends: Symphonia (software), Apple AudioToolbox (hardware),
and Android MediaCodec (placeholder).

### Trait Hierarchy

```
AudioDecoder (generic, associated types)
  -> Symphonia<C>, Apple<C>, Android<C>

InnerDecoder (object-safe, runtime-polymorphic)
  -> Box<dyn InnerDecoder> for DecoderFactory

DecoderInput = Read + Seek + Send + Sync
```

### PCM Output

```rust
pub struct PcmChunk {
    pub pcm: PcmBuf,       // Pool-backed Vec<f32>
    pub spec: PcmSpec,      // { channels: u16, sample_rate: u32 }
}
```

Samples are f32, interleaved (LRLRLR for stereo). Buffer is automatically recycled to
`pcm_pool()` on drop.

### Symphonia Backend

Primary software decoder. Supports all container formats (IsoMp4, MPA, ADTS, FLAC, WAV,
OGG). Two initialization paths:

1. **Direct reader creation** (`container` specified): Creates format reader directly
   without probing. Critical for HLS fMP4 where format is known but byte length is unknown.
   Seek is disabled during init to prevent IsoMp4Reader from seeking to end.
2. **Probe** (`container` not specified): Uses Symphonia's auto-detection. Supports
   `probe_no_seek` for ABR variant switches where reported byte length may not match.

The `ReadSeekAdapter` wraps `Read + Seek` as Symphonia's `MediaSource` with dynamic byte
length updates via `Arc<AtomicU64>`.

### Apple Backend

Uses `AudioFileStream` (parser) + `AudioConverter` (decoder) pattern. Supported on macOS
10.5+ / iOS 2.0+. Container support: fMP4, ADTS, MpegAudio, FLAC, CAF. Provides
encoder delay (priming) info via `kAudioConverterPrimeInfo`.

### Factory

`DecoderFactory::create()` determines the codec from:
1. Explicit `CodecSelector::Exact(codec)`
2. Probing via extension, MIME type, container format mapping
3. `prefer_hardware`: tries Apple/Android first, falls back to Symphonia

### Feature Flags

| Feature | Effect |
|---------|--------|
| `apple` | Enables Apple AudioToolbox backend |
| `android` | Enables Android MediaCodec backend (placeholder) |
| `perf` | Performance instrumentation |
| `test-utils` | Mock trait generation via `unimock` |

---

## kithara-audio

**Path:** `crates/kithara-audio`

Audio pipeline: decoding, effects processing, resampling, and optional rodio integration.

### Pipeline Architecture

```
Stream<T> (Read + Seek)
  -> DecoderFactory creates Box<dyn InnerDecoder>
    -> StreamAudioSource (format change detection, effects chain)
      -> AudioWorker (blocking thread, command handling, backpressure)
        -> kanal channel
          -> Audio<S> (consumer: PcmReader, Iterator, rodio::Source)
```

### Key Types

| Type | Description |
|------|-------------|
| `Audio<S>` | Main pipeline struct. Runs decode worker in separate thread. Implements `PcmReader`, `Iterator`, `rodio::Source`. |
| `AudioConfig<T>` | Builder: stream config, host sample rate, resampler quality, preload chunks, hardware preference. |
| `AudioGenerator` (trait) | Source of PCM chunks (decoder). |
| `AudioEffect` (trait) | Processing transform: `process(chunk) -> Option<PcmChunk>`. |
| `PcmReader` (trait) | Consumer-facing: `read`, `read_planar`, `seek`, `spec`, `is_eof`, `position`, `duration`, `metadata`. |
| `ResamplerProcessor` | `AudioEffect` implementation wrapping `rubato`. Dynamic sample rate, multiple quality levels. |
| `AudioEvent` | `FormatDetected`, `FormatChanged`, `SeekComplete`, `EndOfStream`. |
| `AudioPipelineEvent<E>` | `Stream(E)` or `Audio(AudioEvent)`. |

### Resampler

Wraps the `rubato` crate. Five quality levels:

| Quality | Algorithm | Description |
|---------|-----------|-------------|
| Fast | Polynomial (cubic) | Low-power, previews |
| Normal | 64-tap sinc, linear | Standard |
| Good | 128-tap sinc, linear | Better quality |
| High (default) | 256-tap sinc, cubic | Recommended for music |
| Maximum | FFT-based | Offline/high-end |

The resampler monitors `host_sample_rate: Arc<AtomicU32>` for dynamic rate changes and
uses `fast-interleave` for SIMD-optimized interleave/deinterleave.

### Format Change Handling (ABR)

`StreamAudioSource` monitors `media_info()` from the stream. On change:
1. Detects variant switch.
2. Uses variant fence to prevent cross-variant reads.
3. Seeks to first segment of new variant (where init data lives).
4. Recreates decoder via factory (three-level fallback).
5. Resets effects chain to avoid audio artifacts.

### Preload

Configurable via `preload_chunks` (default: 3). The worker decodes N chunks before
signaling `preload_notify`. Consumer can await this before starting playback.

### Epoch-Based Seek

On seek, epoch is incremented atomically. Worker tags each chunk with epoch. Consumer
discards stale chunks (old epoch), preventing old data from reaching output.

---

## Integration Tests

**Path:** `tests/`

### Test Categories

| Category | What is Tested |
|----------|---------------|
| `kithara_storage/` | Atomic and streaming resource persistence, concurrent read/write, sparse writes, cancellation, failure propagation, large files. |
| `kithara_stream/` | Source seek behavior (Start/Current/End), position tracking, edge cases. |
| `kithara_assets/` | Resource persistence, eviction logic, processing layer (XOR transform), caching, pin persistence across reopens, HLS multi-file scenarios. |
| `kithara_file/` | HTTP file download with seeking, early stream close with Range request recovery, partial cache resume. |
| `kithara_hls/` | Basic playback, session creation, playlist integration (master + media parsing), URL resolution, FetchManager integration. |
| `kithara_net/` | HTTP operations (GET, HEAD, Range), error handling (404, 500, 429), retry with backoff, timeout behavior. |
| `kithara_decode/` | WAV/MP3 decoding with probe, seek tests, stress random seeks, HLS ABR variant switches. |
| `multi_instance/` | 2/4/8 concurrent File and HLS instances, mixed instances, failure resilience (one fails, others unaffected), thread lifecycle. |

### Test Infrastructure

- **rstest**: parameterized tests with fixtures.
- **unimock**: mock trait generation.
- **axum**: HTTP test servers for file and HLS content.
- **tempfile**: isolated test directories.
- **Deterministic PRNG**: Xorshift64 for reproducible stress tests.
- **WAV generators**: sine and saw-tooth patterns.

---

## Cross-Cutting Patterns

### StreamType Marker Pattern

`File` and `Hls` are zero-sized marker types implementing `StreamType`. This enables generic
composition: `Stream<File>`, `Stream<Hls>`, `Audio<Stream<Hls>>`, while sharing all the
orchestration machinery.

### Decorator Pattern

Used in `kithara-assets` (five nested layers) and `kithara-net` (timeout + retry). Each
decorator wraps the previous, implementing the same trait, and can be independently
enabled/disabled.

### Event-Driven Architecture

Each protocol crate emits events via `tokio::sync::broadcast`. Events are forwarded up
through the stack:
```
FileEvent / HlsEvent
  -> AudioPipelineEvent<E>
    -> ResourceEvent
```

### Cancellation

All async operations accept `CancellationToken` from `tokio_util::sync`. The token is
forwarded through the entire call chain: backend -> downloader -> writer -> storage. On
cancellation, operations return `Cancelled` errors and waiters are woken.

### Zero-Allocation Hot Paths

- `kithara-bufpool` provides pool-backed `Vec<u8>` and `Vec<f32>`.
- `Writer` writes directly from network bytes to storage (no intermediate copy).
- `PcmChunk` holds a `PcmBuf` (pool-backed) that auto-recycles on drop.
- `ProcessingAssets` uses 64 KB chunks from the byte pool.
- `fast-interleave` provides SIMD-optimized interleave/deinterleave.

### No Panics in Production

The workspace enforces `clippy::unwrap_used = "deny"`. All production code uses `Result` and
`Option` properly. `unwrap()` and `expect()` are only allowed in tests.

### Thread Model

- **tokio**: async runtime for networking, event forwarding, coordination.
- **rayon** (via `ThreadPool`): blocking work (decode, probe, downloader tasks).
- **kanal**: bounded channels between worker threads and consumers.
- **parking_lot**: fast mutexes and condvars for shared state.
