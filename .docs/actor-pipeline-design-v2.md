# Actor Pipeline Architecture v2: Direct Codec + FSM

> Revised design document incorporating 5-round architecture review debate.
> Original design: `.docs/actor-pipeline-design.md`
> Debate findings: `.docs/debate-review/`

## Problem Statement

HLS seek operations cause hangs, decoder failures, and data corruption.
Three fix attempts failed because each exposed new edge cases in the implicit
coordination between layers. Root causes:

1. **IsoMp4Reader seek broken for virtual byte layout** — Symphonia computes
   byte offsets from fMP4 seek index that don't match our virtual layout
2. **28+ pieces of shared mutable state** touched during a single seek —
   distributed transaction with no single owner
3. **Point fixes create new edge cases** — SeekLayout, variant_fence, seek_epoch
   coordination all added complexity without solving the fundamental problem

## Architecture Overview

Three actors connected by typed messages. No shared mutable state between actors.
Every inter-actor message wrapped in `Envelope<M>` with monotonic `Epoch`.

```
Controller (owns Epoch)
    │
    ├── control ──► Downloader Actor
    ├── control ──► Decoder Actor
    └── control ──► Output Actor
                         │
                    events back to Controller

┌───────────┐  data (bounded)  ┌───────────┐  data (bounded)  ┌───────────┐
│ Downloader │ ───────────────► │  Decoder   │ ───────────────► │  Output   │
│   Actor    │                  │   Actor    │                  │   Actor   │
└───────────┘                  └───────────┘                  └───────────┘
      │                              │                              │
  PipelineInput               FrameExtractor                  resampler
  (Segmented|Linear)         (Segmented|Linear)              + effects
                              + codec decoder                + ring buffer
```

**Channel topology:**
- **Control channels**: unbounded `kithara_platform::sync::mpsc` — Seek/Stop always arrives
- **Data channels**: bounded `tokio::sync::mpsc` — natural backpressure via `.send().await`
- **Event channel**: unbounded, Output/actors → Controller

## Key Design Decisions

### 1. Epoch + Envelope on Every Message

**Most critical invariant.** Without epochs, stale pre-seek data wins races.
Data flows bottom-up (Downloader→Decoder→Output), seeks flow top-down.
Old in-flight data arrives after seek reset and gets decoded/played.

```rust
/// Monotonically increasing seek generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Epoch(u64);

impl Epoch {
    pub fn next(self) -> Self { Self(self.0 + 1) }
    pub fn is_stale(self, current: Epoch) -> bool { self.0 < current.0 }
}

/// Every inter-actor message is wrapped in an Envelope.
pub struct Envelope<M> {
    pub epoch: Epoch,
    pub payload: M,
}
```

**Protocol:**
1. Controller is sole owner of epoch counter
2. Controller increments epoch on every seek
3. Each actor stores `current_epoch`, drops messages where `msg.epoch.is_stale(current_epoch)`
4. If `msg.epoch > current_epoch`, actor syncs to new epoch (resets state)

### 2. Direct Codec Decoders (no FormatReader for fMP4)

Symphonia's codec decoders (AAC, FLAC, etc.) work standalone. We use
Symphonia's atom parsers to extract raw codec frames from fMP4 segments,
then feed them directly to codec decoders via manually constructed `Packet` objects.

**What this eliminates:**
- IsoMp4Reader and its broken seek index
- Virtual byte layout (segments concatenated at computed offsets)
- FormatReader state corruption on variant switch
- The need for `Read + Seek` wrapper around HLS source

### 3. PipelineInput: Two Access Patterns (not one unified trait)

HLS and File have fundamentally different access patterns that cannot be
unified under a single trait without impedance mismatch.

```rust
pub enum PipelineInput {
    /// HLS: discrete segments, init+media, multiple variants, time→segment mapping
    Segmented(Arc<dyn SegmentProvider>),
    /// File: continuous byte stream, Read+Seek for FormatReader
    Linear(Arc<dyn LinearSourceFactory>),
}
```

**Design rules:**
- `SegmentProvider` owns time→segment mapping and async segment fetch
- `LinearSourceFactory` owns AssetStore-backed `Read+Seek` reader creation
- Decoder owns frame-accurate seeking in both paths
- Both paths use AssetStore for caching (project constraint)

### 4. Async Select Actor Loop

All actors use `tokio::select! { biased; ... }` for their main loop.
Control messages are always checked first (biased). Works on both native and WASM.

```rust
loop {
    tokio::select! {
        biased;
        // Priority 1: Control (Seek, Stop) — always checked first
        cmd = control_rx.recv() => { /* handle immediately */ }
        // Priority 2: Data send/recv — cancellable by control
        _ = data_operation => { /* process data */ }
        // Shutdown
        _ = cancel.cancelled() => break,
    }
}
```

**Why biased select is critical:** When decoder is blocked on `data_tx.send(pcm).await`
(output full), a Seek arriving on control channel cancels the blocked send immediately.
This prevents the deadlock that plagues the current shared-state architecture.

### 5. FrameExtractor Strategy Pattern

Decoder actor uses a strategy for container-specific frame extraction:

```rust
trait FrameExtractor: Send {
    fn extract_next(&mut self) -> Result<Option<PcmChunk>, DecodeError>;
    fn seek(&mut self, target: Duration) -> Result<(), DecodeError>;
    fn feed_data(&mut self, data: ByteBuf);
    fn needs_init(&self) -> bool;
    fn feed_init(&mut self, data: ByteBuf) -> Result<(), DecodeError>;
}
```

- `SegmentedExtractor` (HLS fMP4): AtomIterator + direct codec
- `LinearExtractor` (File: MP3/FLAC/WAV/fMP4): Symphonia FormatReader

### 6. Seek as Top-Down Cascade with Epoch

```
1. Controller increments epoch to E2
2. Controller sends Seek{E2} to Output, Decoder, Downloader (all control channels)
   — All arrive immediately (unbounded channels)

3. Output: drains PCM data channel (try_recv, dropping stale),
   drains resampler + effects + ring buffer, sets current_epoch = E2

4. Decoder: biased select cancels any blocked data send,
   resets codec, sets current_epoch = E2

5. Downloader: cancels in-flight fetch via CancellationToken,
   spawns new fetch sidecar for target segment, sets current_epoch = E2

6. New data flows with epoch E2:
   Downloader → Decoder → Output
   Any stale E1 data arriving late is dropped by epoch check
```

### 7. File Playback: Pull-Based with Readiness Gating

For `PipelineInput::Linear` (remote files), the decoder does NOT receive
file bytes as messages. Instead:

1. `LinearSourceFactory::open_reader()` returns `Read+Seek` backed by AssetStore
2. Downloader populates AssetStore progressively, sends readiness notifications
3. Before each `FormatReader::next_packet()`, decoder checks `is_range_ready()`
4. If not ready, decoder awaits `Notify` from storage layer
5. Once ready, synchronous read is guaranteed fast (mmap/memory)

This preserves the current `wait_range`/`contains_range` semantics while
fitting the actor model.

### 8. ABR Variant Switch

Decoder actor compares `CodecParameters` when variant changes:
- Parameters match (99%) → `codec.reset()`
- Parameters differ (codec/sample rate change) → recreate codec decoder

## Content Provider Traits

### SegmentProvider (HLS)

```rust
pub type ByteBuf = PooledOwned<32, Vec<u8>>;

/// Seek plan resolved from a target time. Provider resolves to segment
/// boundaries; decoder computes frame skip.
#[derive(Debug, Clone)]
pub struct SegmentSeekPlan {
    pub target: Duration,
    pub segment_index: u32,
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub variant_index: usize,
    pub sequence: u64,
    pub need_init: bool,
    pub media_info: Option<MediaInfo>,
    pub container: Option<ContainerFormat>,
}

/// Rich segment data with all metadata needed by decoder.
#[derive(Debug)]
pub struct SegmentData {
    pub init: Option<ByteBuf>,
    pub media: ByteBuf,
    pub media_info: Option<MediaInfo>,
    pub container: Option<ContainerFormat>,
    pub segment_duration: Option<Duration>,
    pub segment_index: u32,
    pub sequence: u64,
    pub variant_index: usize,
}

/// HLS segment provider. Wraps FetchManager + AssetStore + PlaylistState.
pub trait SegmentProvider: Send + Sync + 'static {
    fn initial_media_info(&self) -> Option<MediaInfo>;
    fn num_segments(&self, variant: usize) -> usize;
    fn num_variants(&self) -> usize;

    /// Map playback time to segment coordinates.
    fn resolve_seek(&self, target: Duration) -> Result<SegmentSeekPlan, PipelineError>;

    /// Async fetch with cancellation. Uses FetchManager internally.
    fn fetch_segment(
        &self,
        req: Envelope<SegmentFetchRequest>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Envelope<SegmentData>, PipelineError>>;
}
```

### LinearSourceFactory (File)

```rust
/// File source factory. AssetStore-backed Read+Seek for FormatReader.
pub trait LinearSourceFactory: Send + Sync + 'static {
    fn initial_media_info(&self) -> Option<MediaInfo>;

    /// Open a seekable reader positioned at byte_offset.
    /// Backed by AssetStore (disk cache or mmap).
    fn open_reader(
        &self,
        byte_offset: u64,
    ) -> Result<Box<dyn ReadSeekSource>, PipelineError>;
}

pub trait ReadSeekSource: Read + Seek + Send + 'static {}
impl<T: Read + Seek + Send + 'static> ReadSeekSource for T {}
```

### What This Eliminates

| Current | Replaced by |
|---------|-------------|
| `Source` trait (`read_at`, `wait_range`) | `SegmentProvider` / `LinearSourceFactory` |
| `Stream<T>` (`Read + Seek` wrapper) | `LinearSourceFactory::open_reader()` or direct mailbox data |
| `Backend` (async downloader lifecycle) | Downloader Actor IS the lifecycle manager |
| `SharedSegments` (Mutex+Condvar bridge) | Gone — message passing, no shared state |
| `HlsDownloader` (plan+commit+demand) | `HlsContentProvider` impl of `SegmentProvider` |
| `FileDownloader` (progressive+demand) | `FileContentProvider` impl of `LinearSourceFactory` |
| `Timeline` (28+ atomics) | Controller tracks position from OutputEvent |
| `variant_fence` | Not needed — epoch-tagged messages, ordered delivery |

## Message Types

```rust
// --- Controller → Downloader (control, unbounded) ---
pub enum DownloaderCmd {
    Seek { plan: SegmentSeekPlan },
    Prefetch { from: u32, count: u32, variant: usize },
    Stop,
}

// --- Downloader → Decoder (data, bounded) ---
// SegmentData wrapped in Envelope<SegmentData>

// --- Downloader → Decoder (control, unbounded) ---
pub enum DownloadNotice {
    Eof { variant_index: usize },
    Error(PipelineError),
}

// --- Controller → Decoder (control, unbounded) ---
pub enum DecoderCmd {
    Seek { target: Duration },
    Stop,
}

// --- Decoder → Output (data, bounded) ---
// PcmData wrapped in Envelope<PcmData>
pub struct PcmData {
    pub chunk: PcmChunk,  // uses PcmBuf (pool-backed), PcmMeta with epoch
}

// --- Controller → Output (control, unbounded) ---
pub enum OutputCmd {
    Seek,
    Pause,
    Resume,
    Stop,
}

// --- Decoder → Output (control, unbounded) ---
pub enum OutputCtl {
    Flush,
    FormatChange { spec: PcmSpec, media_info: Option<MediaInfo> },
    Eof,
}

// --- Output/Actors → Controller (events, unbounded) ---
pub enum OutputEvent {
    Flushed,
    SeekComplete { time: Duration },
    BufferUnderrun,
    PlaybackStarted,
    Error(PipelineError),
}

// --- Error Protocol ---
pub enum PipelineEvent {
    ActorFailed { actor: ActorId, epoch: Epoch, error: PipelineError, fatal: bool },
    ActorEof { actor: ActorId, epoch: Epoch },
    SeekApplied { epoch: Epoch, position: Duration },
    SeekRejected { epoch: Epoch, error: PipelineError },
}
```

**Critical rule:** EOF must never transport decode failure. They are distinct events.

## FSM: Downloader Actor

```
         Seek (via control)
  Idle ────────────────────► Fetching (async sidecar)
   ▲                              │
   │    sidecar completes         │
   │    (epoch-checked)           ▼
   │                           Idle
   │
   │    Cancel (via CancellationToken)
   ├──────────────────────► Idle
   │
   ▼
 Stopped
```

**Async sidecar pattern:**
- Actor handles Seek/Cancel/Stop on control channel (always responsive)
- Spawns async fetch task with `CancellationToken`
- Fetch result comes back as internal message, epoch-checked on arrival
- Stale fetch results from cancelled fetches are dropped

**Context (owned resources):**
- `input: PipelineInput` — determines Segmented or Linear path
- `decoder_data_tx: DataTx<SegmentData>` — bounded data channel
- `decoder_ctl_tx: CtlTx<DownloadNotice>` — unbounded notices

## FSM: Decoder Actor

```
                    Init(data)
  WaitingInit ─────────────────► Probing
      ▲                            │
      │                   codec params extracted
      │                            ▼
      │   Seek              Ready (FrameExtractor created)
      ├◄────────────────────   │
      │                        │ Data arrives
      │                        ▼
      │                     Decoding ──► PcmData → Output
      │                        │
      │           Seek         │
      ├◄───────────────────────┤
      │                        ▼
      │                     Ready (waits for next data)
      │
      │   codec change detected
      ├◄──── Probing (recreate decoder)
      │
      ▼
   Stopped
```

**FrameExtractor selection:**
- `PipelineInput::Segmented` → `SegmentedExtractor` (AtomIterator + direct codec)
- `PipelineInput::Linear` → `LinearExtractor` (Symphonia FormatReader)

**Readiness gating (Linear path):**
Before calling `FormatReader::next_packet()`, check `is_range_ready()`.
If not ready, await notification from storage layer instead of blocking.

## FSM: Output Actor

```
                Seek (control)
  Playing ──────────────────► Buffering
     ▲                            │
     │    buffer threshold met    │ Pcm(chunk) via data channel
     │                            ▼
     └─────────────────────── Buffering
                                  │
                             Pause│
                                  ▼
                               Paused
                                  │
                            Resume│
                                  ▼
                          Playing / Buffering
```

**PCM processing chain:**
```
Pcm(chunk) → resampler.process() → effects.process() → ring.push()
```

**Seek handling:**
1. Receive `OutputCmd::Seek` on control channel
2. Drain PcmData channel with `try_recv()`, dropping all stale PCM
3. Drain resampler + effects + ring buffer
4. Set `current_epoch` to new epoch
5. Transition to Buffering
6. When first PCM at new epoch fills buffer → Playing + SeekComplete event

## Backpressure & Deadlock Prevention

**The core guarantee:** Control channels are unbounded. Seek/Stop always arrives
regardless of data channel state. `tokio::select! { biased; ... }` ensures
control is checked before data operations. When decoder is blocked on
`data_tx.send(pcm).await`, select! cancels that branch when control arrives.

**Memory pressure:**
- Downloader checks `BytePool::available_memory()` before issuing fetch
- If below threshold, enters Backpressure state
- Waits for BufferReleased notification before resuming
- Every stale drop must release byte credits
- Budget: 32-64 MiB native, 8-16 MiB WASM for segmented prefetch

**Tuning defaults:**
- Downloader→Decoder data channel: capacity = prefetch_count (2-3 native, 1 WASM)
- Decoder→Output data channel: 10 chunks native, 32 chunks WASM
- Expose via `PipelineConfig`, platform-specific defaults

## Migration Plan

### Phase 1: Foundation (Epoch + Infrastructure)
- `Epoch` newtype, `Envelope<M>` wrapper
- `PipelineError` enum with fatal/recoverable distinction
- `PipelineEvent` for actor→controller communication
- `PipelineConfig` for tunable parameters
- Tests: stale-drop, EOF-vs-failure, seek supersession

### Phase 2: Decoder Actor (critical path)
- FrameExtractor trait + SegmentedExtractor (fMP4 AtomIterator + direct codec)
- FSM: WaitingInit → Probing → Ready → Decoding
- All messages use Envelope<M>
- Tests: unit decode, epoch filtering, seek reset

### Phase 3: Downloader Actor + PipelineInput
- SegmentProvider trait + HlsContentProvider (wraps FetchManager + AssetStore)
- LinearSourceFactory trait + FileContentProvider (wraps AssetStore + Net)
- Downloader Actor FSM with async sidecar pattern
- Tests: segment fetch, cancellation, epoch-tagged delivery

### Phase 4: Output Actor
- Resampler + effects chain + ring buffer
- Buffering → Playing, seek drain
- Backpressure via bounded data channel
- Tests: PCM processing, seek drain, format change

### Phase 5: Integration + Controller
- Cascading seek through all three actors
- Controller wiring with epoch management
- Tests: end-to-end seek, stress tests, rapid seek during ABR

### Phase 6: LinearExtractor for Non-fMP4
- Symphonia FormatReader mode for MP3/FLAC/WAV containers
- Readiness-gated actor pattern for remote files
- Tests: file decode, progressive download

### Phase 7: kithara-play Integration
- Engine, Player, QueuePlayer through new pipeline
- Delete old atomics/shared state
- Tests: all 1247 previously passing + 84 previously failing

## WASM Compatibility

- Actor loops use `tokio::select!` — works via `tokio_with_wasm`
- Task spawning via `kithara_platform::tokio::task::spawn`
- Control channels: `kithara_platform::sync::mpsc` (cross-platform)
- Data channels: `tokio::sync::mpsc` (needs WASM integration test as gate)
- Thread spawning: `kithara_platform::thread::spawn` (uses `wasm_safe_thread`)
- No `thread::park/unpark`, no `yield_now` (no-op on WASM)

## Critical Invariants

1. **Epoch on every message** — no stale data survives a seek
2. **Biased select** — control always preempts data
3. **EOF ≠ failure** — distinct events to Controller
4. **Stale drops release resources** — byte credits, pool buffers
5. **Controller is sole epoch owner** — actors validate, never increment
6. **AssetStore for both paths** — HLS and File use same caching infrastructure

## Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Direct codec as part of FSM | Eliminates IsoMp4Reader broken seek, virtual byte layout |
| 2 | Three FSM actors | 3 fix attempts failed due to coordination between ALL layers |
| 3 | Actor model on kithara-platform | WASM compatibility decisive |
| 4 | Epoch + Envelope on every message | **Debate consensus**: most dangerous issue is stale post-seek data |
| 5 | PipelineInput enum (Segmented + Linear) | **Debate consensus**: HLS and File are fundamentally different access patterns |
| 6 | Async select! { biased } actor loop | **Debate resolution**: yield_now is no-op on WASM, spin wastes CPU |
| 7 | Unbounded control + bounded data channels | **Debate consensus**: prevents backpressure deadlock during seek |
| 8 | FrameExtractor strategy pattern | Decoder stays clean "processor", container specifics in strategy |
| 9 | SegmentProvider async with CancellationToken | Wraps existing FetchManager, keeps commit semantics |
| 10 | LinearSourceFactory with readiness gating | Pull-based, reuses wait_range/contains_range semantics |
| 11 | ByteBuf (PooledOwned) not Vec\<u8\> | **Debate consensus**: violates "no buffer allocations" rule |
| 12 | Rich SegmentData metadata | **Debate consensus**: variant, duration, epoch, container needed |
| 13 | EOF distinct from failure | **Codex blocking condition**: current code collapses errors into EOF |
| 14 | Break & rebuild migration | Tests as roadmap, no legacy code in result |

## References

- `.docs/actor-pipeline-design.md` — original design (v1)
- `.docs/debate-review/` — 5-round architecture review
- `.docs/architecture-seek-fsm-analysis.md` — prior FSM analysis (28+ shared state inventory)
- Symphonia author confirmation on manual codec instantiation
