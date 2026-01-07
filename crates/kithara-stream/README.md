# `kithara-stream` — streaming orchestration primitives

`kithara-stream` provides generic orchestration primitives for byte streams in Kithara.
It handles the coordination between async producers (network, disk) and async consumers,
with support for in-band control messages and seek commands.

## Design goals

- **Generic over message types**: Supports custom `Control` and `Event` types defined by higher-level crates
- **Deadlock-free orchestration**: Coordinates writer (fetch → storage) and reader (storage → consumer) tasks
- **Async-first API**: Primary surface is an async `Stream<Item = Result<StreamMsg<C, Ev>, StreamError<E>>>`
- **Seek support**: Best-effort byte seek with clear semantics for seekable vs non-seekable sources
- **Backpressure**: Bounded buffering prevents unbounded memory growth

## Public contract (normative)

The public contract is expressed by the following items re-exported from `src/lib.rs`:

### Core orchestration
- `struct Engine<S>` — Main orchestration engine that manages writer and reader tasks
- `struct EngineHandle` — Handle for controlling a running `Engine` (send seek commands)
- `trait EngineSource` — Source abstraction that `Engine` uses to fetch data
- `type EngineStream<C, Ev, E>` — Output stream type from `Engine`

### Lower-level components
- `struct Writer` — Async writer that accepts byte chunks and writes to storage
- `struct Reader` — Async reader that reads from storage and produces byte stream
- `enum StreamMsg<C, Ev>` — Messages emitted by the stream (Data, Control, or Event)
- `struct StreamParams` — Configuration parameters for stream orchestration

### Error handling
- `enum StreamError<E>` — Generic error type that wraps source errors `E`

## Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Engine (orchestrator)                    │
│  ┌─────────────┐        ┌──────────────┐        ┌─────────┐ │
│  │  EngineSource │──────▶│    Writer    │──────▶│ Storage │ │
│  └─────────────┘        └──────────────┘        └─────────┘ │
│         │                           │                │       │
│         │                    ┌──────▼──────┐         │       │
│         │                    │   Reader    │◀────────┘       │
│         │                    └──────┬──────┘                 │
│         │                           │                        │
│  ┌──────▼──────┐             ┌──────▼──────┐                 │
│  │Control Handle│             │ Output Stream│                │
│  └─────────────┘             └─────────────┘                 │
└─────────────────────────────────────────────────────────────┘
```

### Key invariants

1. **Stop semantics**: Dropping the consumer stream stops the orchestration loop
2. **Handle optional**: Engine continues running even if all control handles are dropped
3. **Backpressure**: Writer blocks when internal buffers are full
4. **Error propagation**: Source errors are propagated as `StreamError<E>` without boxing
5. **Seek best-effort**: `EngineHandle::seek_bytes()` works if source supports it

## Usage patterns

### Basic orchestration
```rust
use kithara_stream::{Engine, EngineSource, EngineHandle, StreamParams};

// Create an EngineSource implementation
let source = MySource::new();

// Build and run the engine
let (engine, handle, stream) = Engine::build(source, StreamParams::default());

// Spawn the engine task
tokio::spawn(engine.run());

// Use the stream
while let Some(msg) = stream.next().await {
    match msg {
        Ok(StreamMsg::Data(bytes)) => { /* process bytes */ }
        Ok(StreamMsg::Control(ctrl)) => { /* process control message */ }
        Ok(StreamMsg::Event(ev)) => { /* process event */ }
        Err(e) => { /* handle error */ }
    }
}

// Seek if supported
if let Ok(()) = handle.seek_bytes(1024).await {
    // Seek succeeded
}
```

### Implementing `EngineSource`
```rust
use kithara_stream::{EngineSource, WriterTask, StreamError};
use bytes::Bytes;

struct MySource;

#[async_trait::async_trait]
impl EngineSource for MySource {
    type Error = MyError;
    type Control = MyControl;
    type Event = MyEvent;

    async fn open(
        &self,
        params: StreamParams,
    ) -> Result<(WriterTask<Self::Control, Self::Event, Self::Error>, Self::Control), StreamError<Self::Error>> {
        // Create writer and return it with initial control state
        Ok((writer, initial_control))
    }

    async fn seek_bytes(
        &self,
        pos: u64,
        control: &mut Self::Control,
    ) -> Result<(), StreamError<Self::Error>> {
        // Implement seek if supported
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        true // or false if not supported
    }
}
```

## Message types

`StreamMsg<C, Ev>` can carry three kinds of payloads:

1. **`Data(Bytes)`** — Raw byte chunks from the source
2. **`Control(C)`** — Control messages (e.g., playlist updates, quality changes)
3. **`Event(Ev)`** — Events mapped from source-specific metadata

This allows higher-level crates to define their own control protocols while
keeping the orchestration layer generic.

## Integration with other Kithara crates

- **`kithara-net`**: Provides HTTP-based `EngineSource` implementations
- **`kithara-hls`**: Uses `Engine` for HLS segment fetching with playlist control messages
- **`kithara-file`**: Uses `Engine` for progressive file downloads
- **`kithara-storage`**: Used by `Writer`/`Reader` for persistent storage

## Error handling philosophy

`StreamError<E>` is generic over the source error type `E`. This allows:

- Source errors to be propagated without loss of type information
- Pattern matching on specific error variants from the source
- No `Box<dyn Error>` in the public API

Common error cases:
- `StreamError::Source(E)` — Wrapped source error
- `StreamError::ChannelClosed` — Control channel was closed
- `StreamError::Cancelled` — Operation was cancelled

## Transition from legacy API

This crate previously contained a legacy `Source`/`Stream` orchestrator. The new `Engine` API
provides:

- Better separation of concerns (writer vs reader)
- Support for in-band control messages
- More explicit error handling
- Cleaner shutdown semantics

Legacy types are still available via re-exports but new code should use `Engine`.