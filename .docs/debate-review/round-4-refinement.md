# Round 4: Architecture Refinement & Reconciliation

## Full Consensus (Codex + Gemini + Claude)

### 1. PipelineInput Enum (not unified trait)

HLS and File are fundamentally different access patterns. Both agree on Codex's enum:

```rust
pub enum PipelineInput {
    Segmented(Arc<dyn SegmentProvider>),  // HLS: FetchManager + AssetStore
    Linear(Arc<dyn LinearSourceFactory>), // File: AssetStore-backed Read+Seek
}
```

- `SegmentProvider` owns time→segment mapping + async fetch
- `LinearSourceFactory` owns AssetStore-backed `Read+Seek` reader creation
- Decoder owns frame-accurate seeking (both paths)
- Both paths use AssetStore for caching

### 2. Epoch Newtype + Envelope

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Epoch(u64);

impl Epoch {
    pub fn next(self) -> Self { Self(self.0 + 1) }
    pub fn is_stale(self, current: Epoch) -> bool { self.0 < current.0 }
}

pub struct Envelope<M> {
    pub epoch: Epoch,
    pub payload: M,
}
```

- Controller is single owner of epoch counter
- Each actor stores `current_epoch`, drops messages where `msg.epoch < current_epoch`
- If `msg.epoch > current_epoch`, actor syncs to new epoch

### 3. Channel Topology

| From | To | Channel | Type |
|------|----|---------|------|
| Controller → Downloader | control | unbounded `kithara_platform::sync::mpsc` |
| Controller → Decoder | control | unbounded |
| Controller → Output | control | unbounded |
| Downloader → Decoder | data | bounded `tokio::sync::mpsc` |
| Downloader → Decoder | notices | unbounded (EOF, Error) |
| Decoder → Output | data | bounded `tokio::sync::mpsc` |
| Decoder → Output | control | unbounded |
| Output → Controller | events | unbounded |

**Key invariant**: Control channels are unbounded — Seek/Stop always arrives even when data channels full.

### 4. Actor Loop: Async Select with Biased Priority

Resolved disagreement: Codex wanted sync + try_recv + yield_now, Gemini wanted async select.
**Winner**: Async `tokio::select! { biased; ... }` — works on both native and WASM.

- Control messages checked first (biased select)
- Data sends use `.send().await` for natural backpressure
- When decoder blocked on data send, select! cancels that branch when control arrives
- No spin-loop, no yield_now (no-op on WASM)

### 5. SegmentProvider Trait

```rust
pub type ByteBuf = PooledOwned<32, Vec<u8>>;

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

pub trait SegmentProvider: Send + Sync + 'static {
    fn initial_media_info(&self) -> Option<MediaInfo>;
    fn num_segments(&self, variant: usize) -> usize;
    fn num_variants(&self) -> usize;
    fn resolve_seek(&self, target: Duration) -> Result<SegmentSeekPlan, PipelineError>;
    fn fetch_segment(
        &self,
        req: Envelope<SegmentFetchRequest>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Envelope<SegmentData>, PipelineError>>;
}
```

### 6. LinearSourceFactory Trait

```rust
pub trait LinearSourceFactory: Send + Sync + 'static {
    fn initial_media_info(&self) -> Option<MediaInfo>;
    fn open_reader(&self, byte_offset: u64) -> Result<Box<dyn ReadSeekSource>, PipelineError>;
}

pub trait ReadSeekSource: Read + Seek + Send + 'static {}
```

### 7. Decoder Strategy: FrameExtractor

```rust
trait FrameExtractor: Send {
    fn extract_next(&mut self) -> Result<Option<PcmChunk>, DecodeError>;
    fn seek(&mut self, target: Duration) -> Result<(), DecodeError>;
    fn feed_data(&mut self, data: ByteBuf);
    fn needs_init(&self) -> bool;
    fn feed_init(&mut self, data: ByteBuf) -> Result<(), DecodeError>;
}
```

- `SegmentedExtractor` (HLS): AtomIterator + direct codec
- `LinearExtractor` (File): Symphonia FormatReader from AssetStore-backed storage

### 8. Backpressure: Credit-Based (Gemini) vs Bounded Channel (Codex)

**Two viable options**, not fully resolved:

**Option A (Codex)**: Bounded `tokio::sync::mpsc` for data, `select!` cancels blocked send on Seek
- Pro: Simple, natural backpressure
- Con: Requires async actor loop

**Option B (Gemini)**: Credits on single channel, `try_recv()` drain + conditional `recv_sync()`
- Pro: Works with sync-only `recv_sync()`
- Con: More complex credit accounting

**Resolution for Round 5**: Since actor loop is async (consensus #4), Option A is cleaner.

### 9. Concrete Message Types

```rust
pub enum DownloaderCmd { Seek { plan: SegmentSeekPlan }, Prefetch { .. }, Stop }
pub enum DownloadNotice { Eof { variant_index: usize }, Error(PipelineError) }
pub enum DecoderCmd { Seek { target: Duration }, Stop }
pub enum OutputCmd { Seek, Pause, Resume, Stop }
pub enum OutputCtl { Flush, FormatChange { spec: PcmSpec, media_info: Option<MediaInfo> }, Eof }
pub enum OutputEvent { Flushed, SeekComplete { time: Duration }, BufferUnderrun, PlaybackStarted, Error(PipelineError) }
```

## Outstanding Issues for Round 5

1. **FormatReader streaming**: How to feed FormatReader incrementally for remote files
2. **WASM async sidecar**: Does tokio runtime work on WASM worker thread?
3. **Credit tuning**: Initial credit count and ring buffer size should be configurable
4. **Prefetch bounds**: Interaction between prefetch queue and BytePool 256MB ceiling
5. **Error propagation**: Complete error flow from any actor to Controller
