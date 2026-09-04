# kithara-analysis — Context

Contracts and invariants for progressive source-signal analysis. The README owns
the overview, features, and type inventory.

## Ownership

This crate owns the reusable per-track analysis engine, generic over
`B: ResamplerBackend`. It consumes decoded source ranges through
`kithara-audio`'s `AudioReader` and neutral `AudioObserver` contracts. Decoder
lifecycle, source readiness, seek/session state, and source discontinuity
publication remain in `kithara-audio`; playback scheduling and effects remain in
`kithara-play`; map/synchronization semantics remain in `kithara-warp`.

The crate owns analysis state, scheduling, waveform/beat DSP, and pure versioned
bytes. It does not own `AssetStore` I/O, cache-key policy, or eviction: the
application persists and reads the composite `TrackAnalysis` bytes through the
caller-owned `Vec<u8>` writer.

`BeatArtifact` is identity-free detector output, not a live beat grid. The only
live grid contract is `kithara-warp::BeatGrid`, whose owning Track/Player
supplies identity and revision. This crate does not fabricate a
`BeatGridSnapshot`: the current detector artifact has confidence and
provenance, but no calibrated maximum frame error suitable for
`FrameUncertainty`. A future adapter must consume that real fact rather than
substitute zero or reinterpret confidence as timing error.

## Track analysis

`AnalyzerBuilder<B, S>` is the public selector. `new(PoolRegion<S>)` requires
the caller to inject its typed region, and `S: HasPool<f32>` makes sample-pool
registration a compile-time requirement. `with_waveform`, `with_beat` - which
requires `B: Default` — and `with_beat_config` select analyzers, and `is_empty()`
lets callers skip scheduling a pass entirely. `TrackAnalyzers<B, S>` is the
crate-private per-track set; it retains one clone of the facade because later
waveform windows and beat runs acquire guards as coverage arrives. Each
analyzer is fed every decoded chunk once.

`TrackAnalysis` is the public snapshot: caller token, revision, rate axis,
extent, coverage, per-artifact fingerprint, waveform, and beat. It is
self-contained by contract: a consumer holding only a snapshot can render the
waveform, place markers on the source timeline, and tell how much of the track
it is based on. `source_frames()` is the denominator that turns a `BeatArtifact`
frame into a fraction: the extent when known, `coverage.frontier()` otherwise.
The frontier is the exclusive end of the highest covered range, not the sum of
covered frames, so sparse out-of-order coverage keeps its source position.

A pass publishes many times. `TrackAnalyzers::snapshot` leaves the pass able to
accept further ranges and bumps a strictly increasing revision, so a consumer
discards anything that does not outrank what it holds. `AnalysisWorker::open`
takes the revision the caller already holds for the token and the pass publishes
above it, so revisions stay monotonic per token across passes. `AnalysisTask` publishes
every `PUBLISH_SECONDS` of newly covered source and once more at end of stream,
keyed to decoded frames rather than wall-clock time. Only that last publication
pins the extent, to the covered frontier. `BeatState` is `Final` only once the
whole known extent is one covered run.

Identity is an opaque `AnalysisToken` the caller opens the pass with; this crate
echoes it and never interprets it. `AnalysisFingerprint` carries the beat tag and
the waveform tag separately, so a waveform resolution change cannot invalidate
stored beat results. The rate axis is named when the pass opens, not discovered
from the first chunk: `AnalysisWorker::analyze` takes it, the caller opens its
reader onto the same one, and a range on another axis is refused rather than
redefining what a frame number means. Analyzers are built lazily by whichever
range arrives first, so a pass that covers nothing allocates nothing. A checked
scratch-allocation failure ends that pass and closes its result channel without
publishing a value. A failure inside one slot closes that slot alone: the
waveform slot drops itself when a buffer cannot grow, and the beat slot drops
itself when a detection, a resample, or a window copy fails, releasing the
audio it held. Either logs the cause once, and the pass reads on to the end
with that artifact absent from what it publishes. The beat slot in particular
must never keep the pass waiting on a detector it cannot feed: what it holds is
the only thing standing between the reader and the rest of the track.

`Coverage` is the canonical record of which source ranges a pass has observed,
kept as sorted, disjoint, non-adjacent runs; `TrackAnalyzers` owns it and every
consumer reads it, there is no second copy. A chunk's range comes from
`AudioChunkInfo::frame_offset` and `frames`, so position never depends on arrival
order. `TrackAnalysis::missing` is derived from that same record rather than
kept beside it; its horizon is the extent once known and the covered frontier
until then, the same rule `source_frames` uses.

### Decode scheduling

`AnalysisTask` does not read its reader in order. It picks the middle of the
largest uncovered range. That becomes binary subdivision on an uncovered track,
so an early snapshot describes the whole track rather than its opening, and
refills holes when playback has mostly covered the track. It schedules against
the beat pass's own coverage while that pass has room, and against what the pass
has merely seen while it does not, so a range the beat pass turned down is read
again and one nothing is waiting for is not.

- **Two extents.** The pass publishes the covered frontier at end of stream, or
  the length the schedule planned against when that is longer, so a range given
  up on past the last covered frame is still reported missing. The schedule works
  from the reader's stated length, bounded by what that reader proved: end of
  stream or a seek answered `PastEof`. That figure is never written into the
  pass, because `TrackAnalyzers::ingest` refuses a range reaching past the extent
  it holds and an under-reported duration would refuse the source's own tail.
- **Run bounds.** A run decodes one schedule chunk before another position is
  chosen. While the beat pass can only continue a run it already has, the run is
  instead unbounded and aimed at the front of the widest gap, so it continues a
  run rather than opening an island the beat pass would turn down. A gap at the
  start of the track has nothing in front of it and is continued from behind:
  read through, it arrives at the covered audio that closes it. A run ends at
  covered audio or the extent. It starts from the first decoded chunk, not `landed_at`: a seek is
  begun rather than completed when it answers, so the decoder resumes at its own
  boundary.
- **End and retirement.** A pass ends when its extent is covered or every
  uncovered position proved unreachable. With nothing to read only because the
  beat pass is full, it waits instead: room comes from the detector, so there is
  nothing to conclude yet. Read in order there is no second chance at a range,
  so the reader waits there rather than reading past a full beat pass. A run
  that decoded nothing new is never chosen again, preventing a coarse-seeking
  source from being asked forever; audio the beat pass turned down says nothing
  about the source, so a position it waits on stays. The cost is explicit:
  retiring the middle of a gap drops that gap from the schedule, and it remains
  reported as missing.
- **Decode error.** It ends the pass without discarding it: delivered ranges are
  published and the rest reported missing.

A source that reports no duration has no middle to seek to, so the task decodes
it in order and never repositions the reader. This is a degraded mode with no
answer for a live stream, not a fallback over a missing correct answer.

## Producer ingest

`producer/` lets a component that must not be slowed down contribute ranges it
has already decoded, so a playing track is not decoded a second time.
`AnalysisProducer` names its pass once when `analyze` hands it back, so offering
costs no lookup and two producers never contend; a track with no open pass has
no handle at all.

`offer` downmixes to mono by the channel mean and copies into a bounded transport
allocated when the pass opens: a sample ring plus a ring of `(start, frames)`
descriptors. It never blocks, never allocates, and never retains the caller's
buffer, so the caller may recycle its buffer as soon as it returns. A range that
does not fit is refused whole and reported untaken; it stays uncovered, missing,
and eligible to be produced again. `Outlet` is deliberately not this transport:
its one-slot overflow cannot report the first refusal.

The `rtsan` lane calls the taken `offer` path inside a forbid-blocking region,
where RTSan aborts on a malloc, free, lock, or syscall. A pass ending drops the
reading half; the next offer reports the pass closed and the caller lets its
handle go. `AnalysisProducer` implements the neutral `AudioObserver` contract,
so playback can attach it without `kithara-audio`, `kithara-play`, or
`kithara-queue` knowing which analyzer consumes decoded ranges. Rejection is
best-effort and never changes playback.

The worker drains the transport on its own tick, where DSP is allowed, and folds
**one block per descriptor**. Contiguous descriptors are never joined:
`Runs::merge` finishes the frontier `MonoStream` and `Runs::open` starts a fresh
one at every push boundary, so beat-resampler segmentation is a pure function of
the producer's own chunk boundaries.

`BeatAnalysisConfig<B>` owns beat tunables and a standalone resampler backend.
Defaults are 1024-frame mono resampler blocks, 22 050 Hz detector input,
30-second detector windows with 2 seconds of overlap, and
`ResamplerQuality::High`. The analyzer never stores whole-track source PCM: it
downmixes to mono and keeps covered spans at detector rate in buffers borrowed
from the caller's typed region.

The contiguous run, not the pass, owns the sequential `MonoStream`. A range
decoded later cannot be pushed through the stream that produced an earlier one:
a run keeps its stream only while it is the frontier, then flushes into its mono
when another segment is appended. Every join is pinned to its implied detector
frame, so rounding cannot accumulate into marker drift. Detector windows are
fixed spans of the absolute detector-rate timeline and are detected once when
complete, regardless of arrival order. Markers therefore agree across arrival
orders within the resampler's splice tolerance.

A run reaching `detector_min_window_seconds` is detected immediately, then
re-detected when its full window fills. Once the extent is known, the artifact is
spread across it at its own tempo while retaining detected marker positions. Run
mono comes from sample guards acquired through `TrackAnalyzers`; the logical run
set opens at most four runs of its own, and holds one more while it reads a
stretch through to a run standing in front of it, while every physical
allocation still competes under the region-wide hard byte budget. Its mono
budget is five of what a run can hold with nothing for the detector to read: a
hop in front of its first window and a window short of ready behind it. A run
releases everything ahead of the window it still waits on, and a run that has
fed no window releases nothing, since the audio in front of its first window
belongs to a window starting before it. The hold therefore follows the detection backlog rather than the track
length. Audio past the budget is turned down rather than given up: it stays
outside the beat coverage, and the pass reads it again once the detector frees
room. That terminates because a hold at its budget always holds a window the
detector can read: every run it may carry, short of a window, is the budget.
Audio the pass did not read for itself extends a run it already has and does not
open one. Downmix and grid-cleanup scratch stay as guards for the pass lifetime;
no lower component constructs or stores another pool facade.

`AnalysisWorker` owns one `kithara-worker` dispatcher and admits every pass as a
separate `AnalysisNode<B, S>` task (absent on wasm32). `AnalysisWorkerConfig`
carries the analyzer builder, parent cancellation, optional shared base `Worker`,
dispatcher capacity and budgets, per-pass priority, and per-pass compute budget.
Without a supplied base it creates and retains a standalone base worker. With a
supplied base, analysis cancellation is OR-composed only onto its dispatcher, so
it cannot cancel sibling domain dispatchers. The backend and pool-schema types
are consumed by `new` and stay inside the task factory rather than leaking into
the handle.

`open` gives every pass a child of the analysis scope; task admission folds that
pass token into the dispatcher's derived task token. Callers may clone it for the
reader before submitting the pass. Results arrive on a `watch` channel: waveform
first, then waveform plus beat when configured; on failure or cancellation the
sender drops without a value. Every node owns one task FSM. Nodes share one
immutable detector runtime; mutable run buffers and detection requests remain
per pass, while bounded compute admission prevents a hidden queue. `Decode`
consumes at most one chunk per tick. Registration wakes the dispatcher; there is
no sleep, backoff loop, or poll watcher. `AnalysisObserver` consumes dispatcher
events, retains the no-progress watchdog, and classifies returned heavy ticks
against a 120-second budget.

**Feature seams.** There is no single `analysis` feature. Artifact types are
unconditional because analysis and cache keys use them even when a pass is
absent. `analysis-waveform` gates the `realfft` analyzer and `with_waveform`;
`analysis-beat` gates the beat path. Without `analysis-beat`, `with_beat()` is a
compile-time no-op and `is_empty()` is the runtime signal.

## Which detector a build carries

`beat-nn` and `beat-dsp` are detector backends above `analysis-beat`, and each
implies it. Neither implies the other; all four combinations are valid.

- `beat-nn` is `BeatThis`, a neural network. It needs a model: `beat-nn-small`
  (10.1 MB), `beat-nn-full` (79 MB) or `beat-nn-full-int8` (22.6 MB), and a
  build carrying `beat-nn` without one does not compile.
- `beat-dsp` is `SpectralBeats`, signal processing: no model bytes, no network
  runtime, compiles for `wasm32`, and reports beats but never downbeats.
- Carrying both selects `BeatThis`.
- Carrying neither still publishes coverage and a waveform, with no beat
  artifact. `with_beat()` drops its configuration and the pass runs on.

The backends are not expected to agree; on the crate's own fixture they track
different metrical levels. `backend::SELECTED_DETECTOR` is the single
expression of which one a build uses, and the fingerprint tag derives from it,
so a grid is never served from cache to a build that would not have produced
it. The neural backend's tag names the model as well, so the three
`beat-nn-*` features are distinct to the cache: a grid the small model made is
not served to a build carrying a full one.

## Waveform

Waveform DSP is synchronous and turns decoded PCM into a `Waveform` for display.
It owns no async, I/O, cancellation, or colour types; those belong to consumers.
Tunables live in `AnalysisParams`.

- Analysis runs on the decoded source signal, never post-EQ, post-timestretch,
  or post-resample output. Playback-rate and mixer transforms only remap the
  time axis and never re-run analysis.
- The optional `AudioObserver` sees playback decoder output before effects. Its
  `AudioChunk::meta.spec` is authoritative and may reflect decoder-side sample
  rate conversion; source-rate offline analysis does not consume this feed.
- `Bucket { low, mid, high }` holds three independent band heights, normalized
  on one shared `[0, 1]` scale. All-zero is silence, never `NaN`.
- `WaveformAnalyzer::push` is position-addressed: it scatters a source block
  into every touched window and reduces a window only once its own frame set is
  complete. Blocks may arrive out of order, overlap, or repeat. Waiting windows
  are capped; evicting one leaves that span uncovered rather than publishing a
  half-silent window.
- Buckets use normalized track position `[0, 1]`, never wall-clock seconds.
  `bucketize` is the sole mapping and always returns exactly the requested count.
  `snapshot` spreads buckets over the known extent, or the highest reduced
  window while the extent is unknown, without consuming the pass.
- Crossovers map to FFT bins using the track sample rate. Each Hann window sums
  low/mid/high energy density (DC zeroed), hops by `fft_size / 4`, keeps the
  component-wise loudest window per bucket, applies `sqrt` and `band_gain`, and
  divides all bands by one shared maximum so the loudness tilt survives.

## Blob codec

`Waveform`, `BeatArtifact`, and composite `TrackAnalysis` use the crate-internal,
domain-agnostic `blob` module for versioned little-endian bytes. `Blob` owns an
artifact frame — `u32` `Blob::VERSION` then body — while each artifact implements
its body. A decode cursor must consume exactly its bytes; trailing data is
corruption.

Each artifact owns its `VERSION`. A mismatch is `BlobError::Version`; a
truncated, mis-sized, or out-of-range body is `BlobError::Corrupt`. Both are
cache misses: the application re-analyzes and overwrites, with no in-place
migration. Untrusted length prefixes are bounded by `MAX_PREALLOC`. `BlobError`
is the public error; `Blob`, `Reader`, and `Writer` stay internal. This codec is
pure bytes: `kithara-app` owns `AssetStore` reads/writes, cache identity, and
policy.

## Progressive analysis file

`AnalysisFile` is the durable, bounded container for one track's progressive
`TrackAnalysis`. `kithara-analysis` owns only the byte layout, validation, and
an ordered `AnalysisFileUpdate`; it owns no filesystem, `AssetStore`, writer
thread, cancellation, or observability. The application owns those lifecycle
boundaries.

- The fixed header records format version, source rate, stable extent, fixed
  chunk size, analyzer fingerprints, latest revision/settled state, and the
  exact payload bounds. A one-byte-per-chunk completion index follows it.
- Exactly one current v6 `TrackAnalysis` payload follows the index at a fixed
  offset. A newer generation replaces that payload and commits its exact new
  length; it does not retain unread historical snapshots.
- An update writes initial header/index bytes only for a new resource, writes
  the complete payload, applies newly completed index bytes, and patches the
  header last. A replacement update first seeds its fixed header/index prefix
  from the prior committed generation; old payload bytes are not copied.
  Publication is the application's successful atomic
  `AssetStore` commit at `final_len`, never an intermediate write. Production
  USDT observation belongs after that commit and is not a correctness signal.
- A fixed index requires a known stable extent. Until a worker publication has
  that extent, `AnalysisFileSpec::for_analysis` returns `UnknownExtent` and the
  snapshot remains memory-only.
- Restore requires the active analyzer fingerprint. The validated header then
  supplies the stored axis, extent, and chunk frames; the application must use
  `AnalysisFileSpec::matches_chunk_duration` to reject a coherent file created
  under another configured duration, and validate axis/extent once current
  source metadata is available.

## Guardrails

- Playback cancellation and scheduling belong to `kithara-play`; source readers
  and observers use only scoped cancellation and wake contracts.
- Prefer explicit FSM or session objects for multi-step control flow; do not
  scatter `pending_*` or shadow flags across source and consumer layers.
- Analysis must not reconstruct HLS/file policy or introduce playback effects.
