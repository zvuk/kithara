# `kithara-stream` — streaming orchestration primitives

`kithara-stream` provides generic orchestration primitives for byte streams in Kithara.
It handles the coordination between async producers (network, disk) and sync consumers (decoders),
with support for seek commands and prefetch buffering.

## Design goals

- **Async Source trait**: Generic async random-access byte reading
- **SyncReader adapter**: Bridges async `Source` to sync `Read + Seek` for decoders
- **Prefetch worker**: Background prefetching with epoch-based invalidation on seek
- **SourceFactory pattern**: Unified API for creating sources (`StreamSource<S>`)
- **Backpressure**: Bounded buffering prevents unbounded memory growth

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core abstractions
- `trait Source` — Async random-access byte source with `wait_range()` and `read_at()`
- `struct SyncReader<S>` — Sync `Read + Seek` adapter over async `Source`
- `struct SyncReaderParams` — Configuration for prefetch buffer size

### Source creation
- `trait SourceFactory` — Factory for creating sources with unified API
- `struct StreamSource<S>` — Wrapper that holds `Arc<Source>` and event channel
- `struct OpenedSource` — Result of opening a source (source + events channel)

### Prefetch infrastructure
- `trait PrefetchSource` — Generic source for prefetch worker
- `struct PrefetchWorker<S>` — Background worker with epoch tracking
- `struct PrefetchedItem<C>` — Chunk with epoch for invalidation
- `struct PrefetchConsumer` — Consumer-side epoch tracking

### Support types
- `enum StreamMsg<C, Ev>` — Messages (Data, Control, Event)
- `struct Writer` — Async writer for download tasks
- `struct Reader` — Async reader from storage
- `enum StreamError<E>` — Generic error wrapper
- `struct MediaInfo` — Codec/container information for decoders

## Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│                    StreamSource<S>                          │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ SourceFactory::open() → OpenedSource                 │   │
│  │   - Arc<SourceImpl>  (implements Source trait)       │   │
│  │   - broadcast::Sender<Event>                         │   │
│  └──────────────────────────────────────────────────────┘   │
└───────────────────────────┬─────────────────────────────────┘
                            │
┌───────────────────────────▼─────────────────────────────────┐
│                    SyncReader<S>                            │
│  ┌────────────────┐    ┌─────────────┐    ┌──────────────┐  │
│  │ PrefetchWorker │───▶│ kanal chan  │───▶│ Read + Seek  │  │
│  │   (async)      │    │ (bounded)   │    │   (sync)     │  │
│  └────────────────┘    └─────────────┘    └──────────────┘  │
│         │                                        │          │
│         │◀────── seek command ───────────────────┘          │
│         │        (epoch increment)                          │
└─────────────────────────────────────────────────────────────┘
```

### Key invariants

1. **Epoch tracking**: Seek increments epoch; stale chunks are discarded
2. **Non-blocking reads**: `SyncReader::read()` uses prefetch buffer, never blocks on I/O
3. **Backpressure**: Bounded channel limits memory usage
4. **EOF signaling**: Empty chunk with `is_eof=true` signals end of stream

## Usage patterns

### Opening a source with StreamSource
```rust
use kithara_stream::{StreamSource, SyncReader, SyncReaderParams};
use kithara_file::{File, FileParams};

// Open source (async)
let source = StreamSource::<File>::open(url, FileParams::default()).await?;

// Subscribe to events
let mut events = source.events();

// Create sync reader for decoder
let reader = SyncReader::new(source.into_inner(), SyncReaderParams::default());
```

### Implementing Source trait
```rust
use kithara_stream::{Source, StreamError, StreamResult, WaitOutcome};
use async_trait::async_trait;

struct MySource { /* ... */ }

#[async_trait]
impl Source for MySource {
    type Error = MyError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        // Wait until range is available or EOF
        Ok(WaitOutcome::Ready)
    }

    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        // Read bytes at offset (non-blocking after wait_range)
        Ok(bytes_read)
    }

    fn len(&self) -> Option<u64> {
        Some(total_length) // or None for streaming
    }

    fn media_info(&self) -> Option<MediaInfo> {
        // Optional: provide codec hint for decoders
        None
    }
}
```

### Implementing SourceFactory
```rust
use kithara_stream::{SourceFactory, OpenedSource, StreamError};

struct MySourceType;

impl SourceFactory for MySourceType {
    type Params = MyParams;
    type Event = MyEvent;
    type SourceImpl = MySource;

    async fn open(
        url: Url,
        params: Self::Params,
    ) -> Result<OpenedSource<Self::SourceImpl, Self::Event>, StreamError<SourceError>> {
        let source = MySource::new(url, params).await?;
        let (events_tx, _) = broadcast::channel(16);
        Ok(OpenedSource {
            source: Arc::new(source),
            events_tx,
        })
    }
}
```

## Integration with other Kithara crates

- **`kithara-file`**: Implements `SourceFactory` for progressive HTTP downloads
- **`kithara-hls`**: Implements `SourceFactory` for HLS streams with ABR
- **`kithara-decode`**: Uses `SyncReader` with Symphonia for audio decoding
- **`kithara-storage`**: Used by sources for persistent caching

## Error handling

`StreamError<E>` is generic over the source error type `E`:

- `StreamError::Source(E)` — Wrapped source error
- `StreamError::InvalidSeek` — Invalid seek position
- `StreamError::UnknownLength` — Seek from end requires known length
- `StreamError::ChannelClosed` — Internal channel closed
