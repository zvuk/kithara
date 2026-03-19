# Actor Pipeline Architecture: Direct Codec + FSM

> Design document for kithara audio pipeline refactoring.
> Replaces virtual byte layout + shared atomics with actor-based FSM pipeline
> using Symphonia codec decoders directly (without FormatReader).

## Problem Statement

HLS seek operations cause hangs, decoder failures, and data corruption.
Three fix attempts failed because each exposed new edge cases in the implicit
coordination between layers. Root causes:

1. **IsoMp4Reader seek broken for virtual byte layout** — Symphonia computes
   byte offsets from fMP4 seek index that don't match our virtual layout
   (`seek past EOF: new_pos=1162117088 len=1860569`)
2. **28+ pieces of shared mutable state** touched during a single seek —
   distributed transaction with no single owner
3. **Point fixes create new edge cases** — SeekLayout, variant_fence, seek_epoch
   coordination all added complexity without solving the fundamental problem

## Architecture Overview

Three actors connected by typed messages. No shared mutable state between actors.

```
Controller
    │
    ▼
┌─────────┐    SegmentData    ┌─────────┐    PcmChunk    ┌─────────┐
│Downloader│ ───────────────► │ Decoder  │ ──────────────► │ Output  │
│  Actor   │ ◄─────────────── │  Actor   │                │  Actor  │
└─────────┘  FetchSegment     └─────────┘                └─────────┘
                                   │                          │
                              fMP4 parser              resampler
                              + codec decoder          + effects
                              (Symphonia atoms         + ring buffer
                               + direct codec)
```

**Data flow**: Downloader fetches segment bytes → Decoder parses container,
extracts raw frames, feeds to Symphonia codec decoder → Output resamples,
applies effects, pushes PCM to ring buffer.

**File = one segment**. Downloader delivers the whole file as a single segment.
Decoder parses it the same way. No special File vs HLS logic.

## Key Design Decisions

### 1. Direct Codec Decoders (no FormatReader)

Symphonia's codec decoders (AAC, FLAC, etc.) work standalone. We use
Symphonia's **atom parsers** (AtomIterator, TrunAtom, EsdsAtom, etc.) to
extract raw codec frames from fMP4 segments, then feed them directly to
codec decoders via manually constructed `Packet` objects.

**What this eliminates:**
- IsoMp4Reader and its broken seek index
- Virtual byte layout (segments concatenated at computed offsets)
- FormatReader state corruption on variant switch
- The need for `Read + Seek` wrapper around HLS source

**Confirmed by Symphonia author:**
> "If you know them ahead of time, you can manually instantiate format readers
> and decoders for specific formats and codecs."

### 2. Actor Model with State-as-Trait

Each actor is an FSM with compile-time enforced state transitions:

```rust
enum Transition<S> {
    Next(S),
    Stop,
}

trait State: Send + 'static {
    type Msg: Send + 'static;
    type Context;

    fn on_message(
        self,
        msg: Self::Msg,
        ctx: &mut Self::Context,
    ) -> Transition<Box<dyn State<Msg = Self::Msg, Context = Self::Context>>>;
}
```

States are consumed on transition (move semantics). Invalid transitions
are compile errors. Actor loop:

```rust
fn run<S: State>(initial: S, ctx: &mut S::Context, rx: Receiver<S::Msg>) {
    let mut state: Box<dyn State<...>> = Box::new(initial);
    while let Ok(msg) = rx.recv() {
        match state.on_message(msg, ctx) {
            Transition::Next(new_state) => state = new_state,
            Transition::Stop => break,
        }
    }
}
```

Lives in: `kithara-actor` crate (depends only on `kithara-platform`).

### 3. Seek as Top-Down Cascade

Seek flows through actors sequentially via regular mailbox messages.
No distributed transaction, no shared epoch/flags.

```
Controller  →  OutputMsg::Seek       →  Output
Output      →  DecoderMsg::Seek      →  Decoder  (drains resampler + ring buffer)
Decoder     →  DownloaderMsg::Cancel →  Downloader (cancels current fetch)
Decoder     →  DownloaderMsg::Fetch* →  Downloader (requests target segment)
Downloader  →  DecoderMsg::Segment   →  Decoder   (data flows)
Decoder     →  OutputMsg::Pcm        →  Output    (PCM flows)
Output      →  ControllerMsg::SeekComplete → Controller
```

Reply via mailbox of the calling actor (no oneshot channels).

### 4. File = One Segment, Unified Pipeline

A file (MP3, FLAC, WAV, fMP4) is a single segment. Both HLS and File
go through the same Downloader Actor via `ContentProvider`.

| | HLS | File |
|---|---|---|
| ContentProvider | `HlsContentProvider` | `FileContentProvider` |
| AssetStore | Yes (per-segment cache) | Yes (whole file cached) |
| Net | Yes (segment fetch) | Yes (progressive download) or local |
| Init segment | Yes (moov with codec config) | fMP4: Yes. MP3/FLAC/WAV: None |
| Media segments | Many (moof+mdat each) | One (whole file) |
| Variants | Multiple (ABR) | One |
| Seek | Fetch different segment | Re-read same segment from offset |
| Container parser | fMP4 atom parsers | fMP4: same. Others: FormatReader |

Decoder actor receives segments and parses them into frames.
It does not know whether the source is HLS or File.

### 5. ABR Variant Switch

Decoder actor compares `CodecParameters` when variant changes:
- Parameters match (99% — same codec, same sample rate) → `codec.reset()`
- Parameters differ (codec change, sample rate change) → recreate codec decoder

No decoder recreation for bitrate-only changes within the same codec.

## Message Types

```rust
// --- Controller ---
enum ControllerMsg {
    SeekComplete { time: Duration },
    PlaybackStarted,
    Error(PipelineError),
}

// --- Downloader Actor ---
enum DownloaderMsg {
    /// Fetch content at the given index (init segment if applicable + media data).
    Fetch { variant: usize, index: usize },
    /// Prefetch upcoming segments (look-ahead).
    Prefetch { variant: usize, from: usize, count: usize },
    /// Cancel current in-flight fetch.
    Cancel,
    /// Shut down the actor.
    Stop,
}

// --- Decoder Actor ---
enum DecoderMsg {
    /// Init segment bytes (fMP4 moov with codec config).
    Init { data: Vec<u8> },
    /// Media segment bytes (fMP4 moof+mdat, or raw file chunk).
    Segment { data: Vec<u8>, index: u32 },
    /// Reset codec state (seek occurred upstream).
    Seek,
    /// Shut down.
    Stop,
}

// --- Output Actor ---
enum OutputMsg {
    Pcm(PcmChunk),
    Pause,
    Resume,
    Seek,
    Stop,
}
```

## ContentProvider: Unified Data Source

The `ContentProvider` trait abstracts both HLS and File data access.
It replaces: `Source` trait, `Backend`, `SharedSegments`, `HlsDownloader`,
`FileDownloader`, `Stream<T>` — the entire sync-async bridge layer.

Both implementations use `AssetStore` for disk/memory caching and `Net`
for network access. The Downloader Actor is protocol-agnostic.

```rust
/// Unified content provider for the Downloader Actor.
///
/// Implementations handle protocol specifics (HLS playlist resolution,
/// file progressive download) while presenting a uniform segment-based
/// interface. Both use AssetStore (caching) and Net (network).
///
/// All methods are blocking (sync). Implementations may use internal
/// tokio runtime for async I/O (FetchManager, HTTP client).
trait ContentProvider: Send + 'static {
    /// Fetch init segment for a variant (fMP4 moov).
    /// Returns None if the format has no init segment (e.g., raw MP3 file).
    fn fetch_init(&self, variant: usize) -> Result<Option<Vec<u8>>, ContentError>;

    /// Fetch media segment by variant and index.
    /// For HLS: one moof+mdat segment.
    /// For File: the whole file content (index=0, variant=0).
    fn fetch_segment(&self, variant: usize, index: usize)
        -> Result<Vec<u8>, ContentError>;

    /// Resolve playback time to a segment target.
    fn resolve_seek(&self, time: Duration) -> Result<SeekTarget, ContentError>;

    /// Total number of segments per variant.
    fn num_segments(&self, variant: usize) -> usize;

    /// Total number of variants (1 for File).
    fn num_variants(&self) -> usize;

    /// Container format (fMP4, MP3, FLAC, WAV, etc.).
    fn container(&self) -> ContainerFormat;
}

struct SeekTarget {
    variant: usize,
    segment_index: usize,
    /// Offset within segment (in codec time units) to skip frames before target.
    frame_offset: u64,
}
```

### HlsContentProvider

```
HlsContentProvider
  ├── FetchManager<N: Net>  — async fetch + cache orchestration (existing, well-tested)
  ├── AssetStore             — disk/memory segment cache
  ├── PlaylistState          — variant/segment metadata (URLs, durations, codecs)
  └── tokio Runtime          — single-thread, for async→sync bridge
```

- `fetch_init(variant)` → `FetchManager::load_init_segment(variant)` → read bytes from AssetStore
- `fetch_segment(variant, index)` → `FetchManager::load_media_segment(variant, index)` → read bytes
- `resolve_seek(time)` → `PlaylistState::find_seek_point_for_time(variant, time)` → segment index + offset
- Reuses **all existing FetchManager/AssetStore/Loader infrastructure** — no rewrite

### FileContentProvider

```
FileContentProvider
  ├── AssetStore             — disk/memory file cache (same infra as HLS)
  ├── Net (optional)         — for remote files (HTTP progressive download)
  └── tokio Runtime          — single-thread, for async→sync bridge
```

- `fetch_init(_)` → `None` for MP3/FLAC/WAV; for fMP4 file → parse moov
- `fetch_segment(0, 0)` → read from AssetStore (local file or downloaded remote)
- `resolve_seek(time)` → byte offset for FormatReader; or segment 0 + frame offset
- `num_segments(0)` → `1` (whole file = one segment)
- `num_variants()` → `1`

### What This Eliminates

| Current | Replaced by |
|---------|-------------|
| `Source` trait (`read_at`, `wait_range`) | `ContentProvider::fetch_*()` — data pushed as messages |
| `Stream<T>` (`Read + Seek` wrapper) | Not needed — Decoder gets data via mailbox |
| `Backend` (async downloader lifecycle) | Downloader Actor IS the lifecycle manager |
| `SharedSegments` (Mutex+Condvar bridge) | Gone — message passing, no shared state |
| `HlsDownloader` (plan+commit+demand) | `HlsContentProvider` + Downloader Actor FSM |
| `FileDownloader` (progressive+demand) | `FileContentProvider` + Downloader Actor FSM |
| `Timeline` (28+ atomics) | Controller tracks position from OutputEvent + PcmMeta |
| `variant_fence` | Not needed — Decoder gets data in order from mailbox |

## FSM: Downloader Actor

```
         Fetch                 Prefetch
  Idle ───────────► Fetching ◄──────────── Idle
   ▲                  │
   │    delivered     ▼
   │               Idle
   │
   │    Cancel
   ├──────────────► Idle (cancel in-flight via CancellationToken)
   │
   ▼
 Stopped
```

**States**: `Idle`, `Fetching { cancel: CancellationToken }`, `Stopped`

**Context** (owned resources):
- `provider: Box<dyn ContentProvider>` — protocol-agnostic data source
- `decoder_addr: Addr<DecoderMsg>` — sends init/segment data to Decoder

**Protocol-agnostic:** Downloader Actor does not know whether it's HLS or File.
It calls `provider.fetch_init()` and `provider.fetch_segment()`. The provider
handles caching (AssetStore), network (Net), and protocol specifics internally.

**Simplification vs current HlsDownloader/FileDownloader:**
- No `active_seek_epoch` — seek = Cancel + new Fetch
- No `download_position` / `force_init_for_seek` — stateless between requests
- No shared `SegQueue<SegmentRequest>` — requests via mailbox
- No `SharedSegments` / `Condvar` — data flows as messages
- One actor for both HLS and File (via ContentProvider)

## FSM: Decoder Actor

```
                    Init(data)
  WaitingInit ─────────────────► Probing
      ▲                            │
      │                   codec params extracted
      │                            ▼
      │   Seek              Ready (codec decoder created)
      ├◄────────────────────   │
      │                        │ Segment(data)
      │                        ▼
      │                     Decoding ──► frames → OutputMsg::Pcm
      │                        │
      │           Seek         │  all frames decoded
      ├◄───────────────────────┤
      │                        ▼
      │                     Ready (waits for next segment)
      │
      │   codec change detected (new Init with different params)
      ├◄──── Probing (new init segment → recreate decoder)
      │
      ▼
   Stopped
```

**States**:
- `WaitingInit` — no codec config yet
- `Probing { init_data }` — parsing init segment for CodecParameters
- `Ready { codec, params }` — codec decoder created, waiting for segments
- `Decoding { codec, params, samples }` — actively decoding frames
- `Stopped`

**Context** (owned resources):
- `output_addr: Addr<OutputMsg>`
- `controller_addr: Addr<ControllerMsg>`
- `downloader_addr: Addr<DownloaderMsg>`

**Segment decoding flow** (inside `Decoding` state):
1. Parse media segment via `AtomIterator` → `MoofAtom` → `TrunAtom` → sample table
2. For each sample: read raw frame bytes from mdat at offset computed from sample sizes
3. Create `Packet::new_from_boxed_slice(track_id, ts, dur, frame_bytes)`
4. `codec.decode(&packet)` → PCM
5. `output_addr.send(OutputMsg::Pcm(chunk))`

**Init segment parsing** (inside `Probing` state):
1. `AtomIterator` → `MoovAtom` → `TrakAtom` → `StsdAtom`
2. `EsdsAtom::fill_codec_params()` (AAC) or `StreamInfo::read()` (FLAC)
3. Construct `CodecParameters` with `extra_data` (ASC / STREAMINFO)
4. Create codec decoder via `CodecRegistry::make_audio_decoder()`

**Seek**:
1. `codec.reset()`
2. Compute target segment from playlist metadata (time → segment index)
3. Transition to `WaitingInit` or `Ready` (if init of same variant already cached)
4. Send `DownloaderMsg::Cancel` + `DownloaderMsg::FetchSegment` to Downloader

**ABR switch**:
1. New `Init(data)` → parse CodecParameters
2. Compare with current `params`
3. Match → `codec.reset()`, stay in `Ready`
4. Differ → recreate codec decoder

## FSM: Output Actor

```
                Seek
  Playing ──────────────► Buffering
     ▲                       │
     │    buffer ready       │ Pcm(chunk)
     │                       ▼
     └───────────────── Buffering
                             │
                        Pause│
                             ▼
                          Paused
                             │
                       Resume│
                             ▼
                     Playing / Buffering
```

**States**: `Buffering { threshold }`, `Playing`, `Paused { was_state }`, `Stopped`

**Context** (owned resources):
- `resampler: Resampler` — sample rate conversion
- `effects: EffectChain` — audio effects pipeline (empty now, extensible)
- `ring: Producer<PcmChunk>` — ringbuf producer half
- `target_sample_rate: u32` — output device sample rate
- `controller_addr: Addr<ControllerMsg>`
- `decoder_addr: Addr<DecoderMsg>`

**PCM processing chain**:
```
Pcm(chunk) → resampler.process() → effects.process() → ring.push()
```

**Backpressure**: bounded channel between Decoder → Output. When ring buffer
is full, Output stops consuming from mailbox, Decoder's `send()` blocks.

**Seek**: drain resampler + effects + ring buffer → `Buffering`.
Forward `DecoderMsg::Seek` to Decoder. When first PCM arrives post-seek
and buffer reaches threshold → transition to `Playing` +
send `ControllerMsg::SeekComplete`.

## Seek Flow: Complete Example

User seeks to 1:30 in an HLS track (37 segments, ~5s each):

```
1. UI → Controller: seek(90s)

2. Controller → OutputMsg::Seek → Output actor
   - Output drains resampler + effects + ring buffer
   - Transition: Playing → Buffering
   - Output → DecoderMsg::Seek { time: 90s } → Decoder

3. Decoder actor:
   - codec.reset()
   - From playlist metadata: 90s → segment #14
   - Transition: Decoding → Ready
   - → DownloaderMsg::Cancel → Downloader
   - → DownloaderMsg::FetchSegment { segment: 14 } → Downloader

4. Downloader actor:
   - Cancels current fetch (if any)
   - Transition: Fetching → Idle → Fetching
   - Fetches segment #14 (from cache or network)
   - → DecoderMsg::Segment(data) → Decoder

5. Decoder actor:
   - Parses segment: AtomIterator → TrunAtom → sample table
   - Skips frames before exact position (90s - start_of_segment_14)
   - codec.decode() remaining frames → PCM
   - → OutputMsg::Pcm(chunk) → Output

6. Output actor:
   - resampler.process() → effects.process() → ring.push()
   - Buffer reaches threshold → Transition: Buffering → Playing
   - → ControllerMsg::SeekComplete → Controller
```

**What disappeared vs current seek:**
- No seek_epoch / is_flushing / 28+ shared atomics
- No virtual byte layout / IsoMp4Reader.seek()
- No distributed transaction — clean message cascade
- Each actor handles seek atomically in its own mailbox

## Symphonia Integration

### Public APIs used (no FormatReader)

| Component | Source | Purpose |
|---|---|---|
| `AtomHeader::read()` | symphonia-format-isomp4 | Parse box headers |
| `AtomIterator` | symphonia-format-isomp4 | Iterate/read atoms |
| `MoovAtom`, `MoofAtom`, `TrunAtom` | symphonia-format-isomp4 | Atom types for init/media segments |
| `EsdsAtom::fill_codec_params()` | symphonia-format-isomp4 | AAC config → CodecParameters |
| `StsdAtom::fill_codec_params()` | symphonia-format-isomp4 | Sample description → CodecParameters |
| `StreamInfo::read()` | symphonia-utils-xiph | FLAC STREAMINFO parsing |
| `CodecParameters` builder | symphonia-core | Construct codec params manually |
| `Packet::new_from_boxed_slice()` | symphonia-core | Create packets for codec decoder |
| `CodecRegistry::make_audio_decoder()` | symphonia-default | Instantiate codec decoder |
| `ReadBytes` trait | symphonia-core | Byte-level I/O for parsing |

### Packet creation

```rust
let packet = Packet::new_from_boxed_slice(
    track_id,       // u32, fixed per track
    timestamp,      // u64, in TimeBase units (typically sample index)
    duration,       // u64, from trun sample_duration
    frame_bytes,    // Box<[u8]>, raw codec frame from mdat
);
let pcm = codec.decode(&packet)?;
```

### Init segment → CodecParameters

**AAC (fMP4):**
```
init.mp4 → AtomIterator → MoovAtom → TrakAtom → MdiaAtom → MinfAtom
         → StblAtom → StsdAtom → mp4a → EsdsAtom
         → fill_codec_params() → CodecParameters { extra_data: ASC bytes }
```

**FLAC (fMP4):**
```
init.mp4 → AtomIterator → MoovAtom → TrakAtom → ... → StsdAtom → fLaC → dfLa
         → StreamInfo::read() → CodecParameters { extra_data: STREAMINFO bytes }
```

## Migration Plan: Break & Rebuild

Tests as roadmap. Break the old pipeline, rebuild piece by piece,
measure progress by test count.

**Baseline**: 1247 pass, 84 fail (seek bugs)

### Phase 0: Preparation
- Feature branch
- Record exact test baseline

### Phase 1: Break
- Delete: `Source` trait, `Stream<T>`, virtual byte layout, `SharedSegments`, `Backend`
- Delete: `HlsSource`, `HlsDownloader` (implementations, NOT tests)
- Delete: `StreamAudioSource`, current seek pipeline
- Delete: shared atomics (seek_epoch, is_flushing, variant_fence, etc.)
- Create: `kithara-actor` crate (State trait, Addr, spawn)
- **Result**: ~0 tests compile, all test code preserved

### Phase 2: Decoder Actor (core)
- fMP4 init parsing → CodecParameters
- fMP4 media segment parsing → frames → codec.decode() → PCM
- FSM: WaitingInit → Probing → Ready → Decoding
- **Tests return**: unit decode tests

### Phase 3: Downloader Actor + ContentProvider
- `ContentProvider` trait in kithara-stream (protocol-agnostic interface)
- `HlsContentProvider` wrapping FetchManager + AssetStore + PlaylistState
- `FileContentProvider` wrapping AssetStore + Net (local/remote)
- Downloader Actor FSM: Idle → Fetching, Cancel
- **Tests return**: download tests, fixture server tests, file download tests

### Phase 4: Output Actor
- Resampler + effects chain + ring buffer
- Buffering → Playing, backpressure
- **Tests return**: audio pipeline tests

### Phase 5: Seek + Integration
- Cascading seek through actors
- Controller wiring
- **Tests return**: seek tests, stress tests — including previously failing 84

### Phase 6: FormatReader Mode for Non-fMP4
- Decoder Actor second mode: FormatReader for MP3/FLAC/WAV containers
- FileContentProvider already delivers file bytes (from Phase 3)
- **Tests return**: file decode tests, progressive download tests

### Phase 7: kithara-play Integration
- Engine, Player, QueuePlayer through new pipeline
- **Tests return**: integration tests, WASM tests

**Completion criteria**: all 1247 previously passing tests green +
84 previously failing tests also green.

## Assumptions

1. Symphonia codec decoders work correctly standalone with manual Packets —
   confirmed by author and API research
2. All HLS variants of one track share the same sample rate —
   decoder recreation is rare
3. Frame-level parsers for MP3/FLAC/WAV/ADTS/OGG can be added incrementally —
   HLS fMP4 is the priority
4. `kithara_platform::sync::mpsc` is sufficient for actor mailbox on all platforms
5. Mixer/effects/crossfade (kithara-play) are not affected by this refactoring

## Decision Log

| # | Decision | Alternatives | Rationale |
|---|----------|-------------|-----------|
| 1 | Direct codec as part of FSM | Separate phases; simultaneous | Direct codec is enabler for FSM, building FSM on virtual byte layout is pointless |
| 2 | All three FSMs: Downloader + Decoder + Output | Only two; only Decoder | 3 point-fix attempts failed due to coordination between ALL layers |
| 3 | Actor model | Typed channels; channels + shared read-only | Formal isolation, proven pattern for FSM |
| 4 | Thin abstraction over kithara-platform | ractor; kameo; xtra/troupe | WASM compatibility is decisive, kithara-platform already cross-platform |
| 5 | Decoder actor owns fMP4 parser | Downloader parses; separate Parser actor | Single responsibility "segment → PCM", Downloader doesn't know containers |
| 6 | Seek as top-down cascade | Broadcast; two-phase commit | Sequential pipeline, not distributed transaction |
| 7 | File = one segment, unified path | Two separate paths; File via FormatReader | Maximum unification, FormatReader not used anywhere |
| 8 | ABR: recreate codec only on parameter change | Always recreate; never recreate | Safe CodecParameters check, reset() in 99% of cases |
| 9 | Symphonia atom parsers without FormatReader | Own fMP4 parser; FormatReader as packet extractor | Parsers ready (AtomIterator, TrunAtom, EsdsAtom), confirmed by Symphonia author |
| 10 | Reply via mailbox, no oneshot | oneshot channels; no reply at all | Clean actor model, all via typed messages |
| 11 | State-as-trait for FSM | Single handle(); per-message Handler\<M\> | Compile-time transition guarantees, move semantics for states |
| 12 | Output actor = resampler + effects + ring buffer | Only ring buffer; separate actors | Same thread, single processing chain |
| 13 | Break & rebuild migration | Incremental; parallel pipeline | No legacy code, tests as roadmap, clean result |
| 14 | ContentProvider trait replaces Source+Backend | Actor wraps Source; Actor alongside Source | Eliminates shared mutable state (SharedSegments, Condvar, variant_fence). Unifies HLS+File under one interface. Both use AssetStore for caching. |
| 15 | ContentProvider in kithara-stream | In kithara-hls; new crate | kithara-stream is the I/O layer — ContentProvider is data access. Avoids circular deps. |

## References

- `.docs/architecture-seek-fsm-analysis.md` — prior FSM analysis (28+ shared state inventory)
- `.docs/offset-architecture-design.md` — prior offset refactoring (SeekLayout, failed)
- `.docs/offset-refactor-journal.md` — design decision history
- Symphonia author quote on manual instantiation of readers/decoders
- Symphonia 0.5.5 public API: AtomIterator, TrunAtom, EsdsAtom, Packet, CodecParameters
