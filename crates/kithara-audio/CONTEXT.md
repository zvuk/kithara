# kithara-audio — Context

Detailed contracts and invariants for the kithara-audio crate; the README is the overview.

## Threading model

```mermaid
flowchart TB
    subgraph "Consumer Thread"
        App["Application code"]
        AS["Audio&lt;S&gt;<br/>(impl PcmReader)"]
        App -- "read(buf)" --> AS
    end

    subgraph "AudioWorker (shared OS thread)"
        Sched["runtime::Scheduler"]
        DN["DecoderNode<br/>(per-track impl Node)"]
        Resampler
        Effects["effects/<br/>(per-track AudioEffect chain)"]
        Sched --> DN --> Effects --> Resampler
    end

    subgraph "Downloader (tokio)"
        DL["kithara-stream::dl::Downloader"]
    end

    subgraph "Shared state"
        SR["StorageResource<br/>(kithara-storage)"]
        Bus["EventBus<br/>(kithara-events)"]
    end

    DL -- "fetch + writer()" --> SR
    DN -- "wait_range / read_at" --> SR
    Resampler -- "PcmChunk" --> Ring["ringbuf::HeapRb&lt;PcmChunk&gt;"]
    AS -- "try_pop()" --> Ring
    DN -- "AudioEvent" --> Bus
    Bus --> App
```

- **AudioWorker (shared OS thread)**: an internal priority scheduler in `runtime/` ticks each registered track. Each track is a single `Node` (`DecoderNode`) — effects run as direct operator calls inside the node, not as separate `Node`s with ring buffers between them.
- **Downloader (tokio)**: lives in `kithara-stream::dl`. It owns the HTTP pool and writes bytes directly into the `StorageResource` the `DecoderNode` reads from. The downloader is not spawned by `kithara-audio`.
- **Ring**: a lock-free `ringbuf::HeapRb<PcmChunk>` carries processed PCM from the worker to the consumer; backpressure is enforced by the ring's capacity and an `Outlet` overflow slot.
- **Ring wake**: the producer push and the consumer park form a lock-free wait protocol guarded by a `SeqCst` fence pair (a Dekker StoreLoad barrier) so a just-parked consumer is never missed.
- **Trash ring (spent-chunk return)**: the consumer (`Audio`) runs on the caller's real-time audio thread, so it must never `free`. Returning a `PcmChunk`'s pooled buffer to `kithara-bufpool` can deallocate (shard full, or trim), so the consumer never drops a consumed chunk: it pushes every spent chunk back through a second lock-free ring, and `Node::recycle` drops them on the worker thread on its next tick. The checked worker produce core follows the same rule: `DecoderNode` retains seek-invalidated overflow until `Node::recycle`, while `DecodeCore` retains seek-invalidated gapless chunks until `flush_reader_signals`. The ring is sized `pcm_buffer_chunks + 2` — enough to absorb a seek that drains the whole forward ring at once — so the real-time push is infallible and no buffer is ever freed on the audio thread.
- **Preload gate (`PreloadGate`)**: the one-time startup signal that releases the async consumer's `Resource::preload().await`. The worker is a plain OS thread, not a tokio runtime worker, so it must never run a cross-thread tokio-task `wake()` (that schedules through tokio's inject queue — a lock + futex, real-time-unsafe). The gate is decoupled: the worker only does a lock-free `ready_epoch.store(epoch, Release)` plus `ready.store(true, Release)` via `signal_epoch(epoch)`; the async awaiter (`PreloadGate::wait_for_epoch`) polls with `Acquire` and re-arms its own runtime timer (`POLL_INTERVAL`) while the gate is closed, so the worker never drives the wakeup. `DecoderNode` opens the gate at every preload terminal site — the preload-chunk threshold, EOF, `Failed`, and `on_cancel` — using its producer runtime epoch, and `rearm()`s (re-closes) it in `sync_seek_epoch` so a post-seek wait blocks again until that epoch refills. A stale pre-seek signal must not release a post-seek waiter; a missed signal would stall the consumer before first audio, so all terminal arms must fire it with the epoch they actually produced.
- **Events**: every layer publishes into a unified `EventBus` (`AudioEvent`, `HlsEvent`, `FileEvent`, ABR events).
- **Epoch-based seek invalidation**: each seek bumps an `AtomicU64` epoch; stale chunks tagged with an older epoch are dropped before reaching the ring.
- **`block_on_underrun` thread contract**: with `AudioConfig::block_on_underrun(true)` a `read()` on an empty ring PARKS the calling thread until the worker produces (instead of returning `Pending`). The consumer must therefore live on a thread the async stack does not depend on — an audio callback, a dedicated thread, or `kithara_platform::tokio::task::spawn_blocking`. Calling a parking `read()` on a tokio runtime thread (e.g. directly in a current-thread `#[kithara::test(tokio)]` body) starves the very downloader/peer tasks that feed the ring: under the real clock this degrades fetch dispatch, under `flash` it deadlocks (a woken-but-unpollable task holds `active_async`, freezing the virtual clock).

## Pipeline Architecture

```mermaid
flowchart LR
    ST["Stream&lt;T&gt;<br/>(Read + Seek)"]
    DF["DecoderFactory<br/>Box&lt;dyn Decoder&gt;"]
    Node["DecoderNode<br/>(impl Node, runtime/)"]
    AW["AudioWorker<br/>(shared OS thread)"]
    Ring["ringbuf<br/>(lock-free)"]
    A["Audio&lt;S&gt;<br/>(impl PcmReader)"]

    ST --> DF --> Node --> AW --> Ring --> A
```

## Refactor Transition Contract (2026-07)

This section is the transition contract for refactoring waves W0-W12. Once the
refactor is complete, these boundaries become the regular owner contracts and
this transition section is folded into the corresponding subsystem sections.

State has one domain owner:

- `RingConsumer` owns `pcm_rx`, `validator`, `current_chunk`, and `trash_tx`;
  `begin_seek_epoch` keeps seek drain and stale-chunk handling inside its RT
  no-free boundary.
- `SeekEngine` owns `resume_target` and is the only writer of the producer
  decode epoch, through `commit_decode_epoch`.
- `ResumeCursor` owns `decode_head` and resolves recreate resume positions from
  the current epoch, committed position, and `resume_target`.
- `DecodeCore` owns the complete `DecoderSession` and replaces it atomically;
  it also owns gapless processing, time-domain effects, and EOF drain.
- `FormatPolicy` makes pure `detect` and `handle_variant_change` decisions and
  returns `RecreateState` without starting recreation itself.
- `ReadinessGate` is the only owner of byte-range calculations used by both
  gate and wait paths. Those paths must use the same range and source phase.
- `RebuildPort` owns the two-phase recreation submission boundary: preparation
  produces a pending job, and the coordinator submits it outside the RT step.
- `RetiredDecoders` owns `retire` and off-RT `drain`; decoder destruction does
  not occur in the RT region.
- `RouteChange` detects real host-rate changes. It enters the same
  `RecreateState` machine as `FormatBoundary` and `VariantSwitch`; it is not a
  separate lightweight recreation path.
- `StreamHandle` and `SharedStream` own byte-space and fence ground truth.
  Other owners do not clone byte-range policy.
- `StreamAudioSource` remains a thin coordinator. It dispatches work and is the
  sole track-state mutator through `update_state`; it contains no domain logic.

Sub-owners never accept `&mut StreamAudioSource`. They receive disjoint
`SeekApplyCtx`, `DecodeCtx`, `ReadinessCtx`, `RebuildCtx`, or `StepCtx`
borrows, or return `DecodeAction`, `SeekTransition`, or `RecreateState` for the
coordinator to apply through `update_state`.

The target surface changes are contractual: `worker/` becomes `renderer/`;
`PcmReader` splits into `PcmRead`, `PcmSession`, and `PcmControl`, with
`PcmReader` retained as the umbrella trait; all 13 open-field structures gain
private fields with builders or accessors; the synchronous `analyze_reader`
surface is retired and `AnalysisWorker` is the public analysis entry point;
`AUDIO_WORKER_ID` is removed.

`runtime::Node::rt_policy()` defaults to RT, while `AnalysisNode` declares
`Heavy`. The `rtsan` forbid-blocking region applies to RT nodes instead of the
generic `produce_pass` path.

## Track analysis (shared worker)

`analysis/` owns the reusable per-track analysis engine consumed by the demo app
today and by mobile surfaces (kithara-ffi) next:

- **`Analyzer` / `TrackAnalyzers`** — crate-private streaming analyzer pieces.
  Each analyzer is fed every decoded chunk once; `TrackAnalyzers` stages
  completion into the public `TrackAnalysis`. Alongside waveform, beat grid,
  and source-frame count, worker-produced snapshots retain the decoded source
  sample rate that defines the frame axis. One pass accepts one stable decoded
  source format; a mid-pass channel or sample-rate change aborts that analysis
  instead of combining incompatible frame axes.
- **`AnalyzerBuilder`** — the public builder for selecting analysis passes.
  `with_waveform(buckets)` enables waveform extraction, `with_beat()` enables
  the NN beat slot when `beat-nn` is compiled and the model loads, and
  `is_empty()` lets callers skip scheduling an analysis pass.
- **Beat-analysis config.** `AnalyzerBuilder::with_beat_config` carries the
  implementation-affecting beat tunables. The defaults preserve the current
  detector contract: 1024-frame mono resampler blocks, 22 050 Hz detector input,
  30-second detector windows with 2 seconds of overlap, and
  `ResamplerQuality::High` for the configured mono resampler backend. The analyzer never
  stores whole-track PCM for beat detection; it downmixes/resamples into a
  bounded detector window, runs the detector as each window becomes ready,
  offsets the window-relative events, and only keeps raw event times for final
  grid cleanup. Beat PCM scratch buffers (the detector window and mono
  resampler blocks) come from the `PcmPool` owned by `AnalyzerBuilder`
  (`with_pcm_pool` lets consumers inject their app-wide pool).
  `BeatAnalysisConfig` carries both numeric beat-analysis tunables and a
  standalone resampler backend handle. When beat analysis is compiled in, that
  handle uses the portable `kithara-resampler` default backend order (Rubato,
  then Glide, then none). Callers may inject any compiled
  `ResamplerBackend`, including platform backends exposed by decode.
- **`AnalysisWorker` / `AnalysisNode`** — a thin public handle over a second
  `runtime::Scheduler` named `kithara-analysis` and its single long-lived,
  `Idle`/`Heavy` node. Jobs carry caller-owned cancel tokens that must be
  children of the same scope that owns the worker (one cancel hierarchy);
  the caller keeps at most one job in flight and cancels the previous token
  to preempt. Results arrive on a `watch` channel; on failure/cancel the
  sender drops without a value. The node owns the queued-job receiver, the
  current reader/analyzers FSM, and the one `Box<dyn BeatDetector>` loaded at
  builder construction; detector ownership is never shared or locked.
  `Decode` consumes at most one chunk per tick, `Pending` yields
  `UpstreamPending`, and EOF advances through separate waveform-emission and
  beat-finalization ticks. The platform scheduler park is flash-visible, and
  `AnalysisWorker::analyze` wakes it after enqueueing; there is no sleep,
  paced-backoff loop, or real-thread poll watcher.
- **Analysis heavy-tick budget.** `AnalysisObserver` retains the normal
  no-progress watchdog and separately classifies returned heavy ticks against
  a 120-second budget. A detector call is still indivisible and may occur when
  a bounded mono window fills or during final flush. The observer can report
  an over-budget call only after it returns; it cannot interrupt a detector
  that never returns. Making detection itself cooperative requires a split
  detector API and remains outside this contract.
- **Feature seams.** There is no single `analysis` cargo feature. Artifact
  types (`Waveform`, `BeatGrid`, `Bucket`, `GridSegment`, `AnalysisParams`) are
  unconditional because region/stretch logic and cache keys use them even when a
  pass is absent. `analysis-waveform` gates only the `realfft` waveform
  analyzer. `analysis-beat` gates the beat analyzer/worker path. Its
  mono-resampler is built through `kithara-resampler` from
  `BeatAnalysisConfig`'s backend. `beat-nn` is a detector backend layered on top
  of `analysis-beat`.
- **Runtime switch.** Consumers use `AnalyzerBuilder::is_empty()` as the runtime
  signal to skip scheduling. When `analysis-beat` is absent,
  `AnalyzerBuilder::with_beat()` is a compile-time no-op. Apple FFI device
  builds intentionally omit `analysis-beat`; NN-only beat fallback for that set
  is future work, not a hidden runtime fallback.

`TrackBeatMap` is the immutable musical-coordinate view derived from a
`TrackAnalysis`. It converts analysed source-rate markers onto an explicit
host-rate `SourceFrame` axis and maps them piecewise-linearly to `TrackBeat`.
Actual beat markers define phase; the grid's aggregate BPM estimate is never
used to fabricate marker positions. The first analysed downbeat is beat zero,
so pickup beats remain representable as negative coordinates. Repeated,
unordered, insufficient, or off-grid markers fail with `BeatMapError` instead
of producing a guessed map. Markers beyond the decoded source extent are also
invalid, and mapping is available only between the first and last analysed
markers; neither boundary is extrapolated. Meter is exposed only when
consecutive downbeat spans agree.

## Bounded Source Ranges

`SourceRange` indexes the transport-neutral signal after gapless normalization
and fixed host-rate conversion, but before time stretch and custom playback
effects. Session beats, tempo, direction, bindings, and render context never
cross this boundary.

`Audio` is the only owner of bounded decoded reads. It starts each range through
the canonical `SeekState` transaction with `SeekIntent::Reposition`; a
`SourceRangeRequest` carries that exact epoch, and a newer playback seek or
range invalidates it. The reposition uses the normal seek, decoder, and worker
path but owns no application-visible seek lifecycle. The decoder switches one
resource between exclusive linear and bounded read modes, and both modes use
the same decoder, worker, PCM ring, gapless metadata, trash ring, and `PcmPool`.
There is no source sidecar worker, cache, port protocol, event bus, or second
seek generation.

The caller supplies a fixed interleaved destination and polls
`read_source_range` without blocking. `Audio` copies exact chunk intersections,
rejects format changes, gaps, malformed chunk shapes, request mismatches, and
stale epochs with typed errors, and returns every consumed chunk through the
existing trash ring before propagating an error. Natural EOF remains distinct
from decoder failure. Readers without canonical decoded coordinates report the
typed unsupported capability instead of fabricating a second signal path.

## Waveform

Pure, synchronous DSP that turns decoded PCM into a `Waveform` for display. No
async, I/O, cancel, or color types live here - `kithara-audio` is math only;
the band -> color mapping and orchestration live in the consumer crates.

- **Types** (`waveform/`): `Bucket { low, mid, high }` are three independent
  per-bucket band heights, each normalized `[0, 1]` on one shared scale. They are
  not a single bar plus a color: the deck paints them as three concentric
  mirrored bars (low behind, high in front, Serato-style overlay) so all three
  bands are visible at once; the tallest band is the outer hull. All-zero is
  silence. `Waveform` is a sealed `Arc<[Bucket]>` with `buckets()` / `len()` /
  `is_empty()` accessors (no bare slice deref).
- **Sealed construction**: a `Waveform` is obtainable only via
  `WaveformAnalyzer::finalize`, so its invariants hold by construction.
- **Normalized-position index** (`bucketize`): buckets are indexed by normalized
  track position `[0, 1]`, never wall-clock seconds. `bucketize` is the single
  home of that `[0, 1]` mapping: bucket `b` folds the raw range
  `[b*R/N, (b+1)*R/N)`. A bucket whose range is empty (tracks shorter than the
  bucket count) is filled with the supplied `empty` value, so the output length
  always equals the requested bucket count.
- **Source-only invariant**: analysis runs on the decoded SOURCE signal, never
  the post-EQ / post-timestretch / post-resample output. The waveform is the
  track's identity; playback-rate and mixer transforms remap only the time axis
  and never re-run analysis.
- **PCM <-> frequency boundary**: `WaveformAnalyzer::new` takes the track
  `sample_rate` because band crossovers map to FFT bins via
  `bin_hz = sample_rate / fft_size`. Constant sample rate per track is assumed;
  build the analyzer once the first chunk's `PcmSpec` is known.
- **Silence rule**: a silent bucket is `Bucket::default()` (all-zero), renders as
  nothing, and never produces `NaN`.
- **Reduction**: per FFT window, band energy is summed into low/mid/high
  (DC bin zeroed; windows below `energy_floor` RMS contribute nothing) and each
  band is divided by its bin count, i.e. an energy DENSITY (RMS-like). Without
  that, the wide mid/high bands outweigh the narrow low band by sheer bin count
  and mid becomes the hull; the density form lets bass be the hull as it should.
  Windows overlap, hopped by `fft_size / 4` (75% Hann overlap), so the spectral
  series is not coarser than the bucket count and a normal-length track is covered
  end to end; only genuinely short tracks fall back to a single zero-padded window.
  `finalize` keeps each bucket's loudest window (component-wise max), takes
  `sqrt` to magnitude, applies the per-band perceptual `band_gain`, then divides
  all three by one shared global max. Shared (not per-band) normalization keeps
  the loudness tilt - bass stays the dominant hull and quiet stays quiet - while
  `band_gain` lifts mid/high, which music tilts toward silence, into visibility
  without inverting the hierarchy. Tunables (`fft_size`, crossovers,
  `energy_floor`, `band_gain`) live in `AnalysisParams`.

## Blob codec

Analysis artifacts that persist to the on-disk cache (`Waveform`, `BeatGrid`)
share one versioned little-endian encoding via the internal crate-level `blob`
module (domain-agnostic, not waveform-specific). The `Blob` trait owns the
format frame; each artifact only implements its body.

- **Frame**: `Vec::<u8>::from(&artifact)` writes a `u32` version header
  (`Blob::VERSION`) then the artifact body; `Artifact::try_from(&[u8])` checks
  the version, decodes the body, and requires the cursor to consume the blob
  exactly (trailing bytes are corruption).
- **Versioning**: each artifact owns its `VERSION` constant. A mismatch is a
  typed `BlobError::Version`; a truncated, mis-sized, or out-of-range body is
  `BlobError::Corrupt`. Both are cache misses — the caller re-analyses and
  overwrites. There is no in-place migration of old blobs.
- **Boundary**: `BlobError` is the only piece that crosses the crate boundary
  (it is the public `TryFrom<&[u8]>` error). `Blob`, `Reader`, and `Writer` are
  crate-internal; consumers serialize through the artifacts' `From<&Self> for
  Vec<u8>` / `TryFrom<&[u8]>` impls. The composite track-analysis blob (version +
  config fingerprint + per-artifact sections) is a separate app-layer concern
  owned by `kithara-app`, not this codec.

## Resampler Quality Levels

<table>
<tr><th>Quality</th><th>Algorithm</th><th>Use Case</th></tr>
<tr><td>Fast</td><td>Polynomial (cubic)</td><td>Low-power, previews</td></tr>
<tr><td>Normal</td><td>64-tap sinc, linear</td><td>Standard playback</td></tr>
<tr><td>Good</td><td>128-tap sinc, linear</td><td>Better quality</td></tr>
<tr><td>High (default)</td><td>256-tap sinc, cubic</td><td>Recommended for music</td></tr>
</table>

## Sample-rate Conversion

Sample-rate conversion for playback is decoder-owned. `AudioConfig.decoder`
carries `AudioDecoderConfig`, including `DecoderResamplerSettings`; the audio
pipeline combines that with `AudioConfig.host_sample_rate` and threads the
result into `DecoderConfig.resampler`. The effect chain is for time-domain
processing such as stretch and custom effects, not fixed-ratio sample-rate
conversion. Speed and key-lock live in the time-stretch slot below and never
change the sample-rate-conversion ratio.

`AudioDecoderConfig<B>` carries an optional `DecoderResamplerSettings<B>`.
When present, `DecoderResamplerSettings.backend` is the concrete
`B: kithara_resampler::ResamplerBackend`; when absent, the decoder emits source
rate PCM. `DecoderResamplerSettings.options` carries
`kithara-resampler::ResamplerOptions` into that backend. The backend
implementation is owned by `kithara-resampler`, not by `kithara-audio`; this
crate does not choose a portable default backend. On Apple targets, callers
that want standalone Apple PCM resampling inject
`kithara_resampler::apple::AppleAudioConverterBackend` configured with
`kithara_resampler::apple::AudioToolboxConverterFactory` through the same typed decoder
config surface. The shipped playback preset in `kithara-play` uses Rubato on
default builds, 4096-frame process blocks, 0.0001 host-rate ratio tolerance,
and an 8.0 max-ratio-adjustment window. These are implementation tunables, not
hidden constants in the processing code. Quality and backend family remain
separate decisions.

`apple-fused-src` is the Apple device path. When the selected decoder backend is
Apple AudioToolbox, `AudioConfig.host_sample_rate` is threaded to
`DecoderConfig.resampler` with a typed backend selected by the caller. The
current adapter path wraps decoded PCM with the selected standalone backend so
the decoder object exposes target-rate PCM without adding a playback effect
stage. Codec-embedded Apple planning remains the Apple host-specific extension
point.

`resample-rubato` enables the Rubato backend type.
`resample-glide` enables the Glide backend type. Selecting either backend is a
typed config decision, not a runtime fallback chain.

The decoder resampler's output capacity remains a correctness invariant: the
configured backend reports `output_frames_for_input` in the ceil frame domain
and the decoder adapter sizes buffers from that contract. It is not a
user-facing configuration knob.

Route changes that actually change the host sample rate store the new rate.
For every backend, `kithara-audio` has exactly one route-change trigger,
`start_route_change_recreate_if_needed`, which drives the full decoder recreate
state machine with `RecreateCause::RouteChange`, the same machine used by
`FormatBoundary` and `VariantSwitch`. For a standalone resampler backend, the
decode factory may reuse the codec decoder and install a fresh
`ResampledDecoder` layer; that is a decode-factory outcome, not a separate
lightweight path in `kithara-audio`. Equal-rate notifications recreate
nothing.

## Time-Stretch (speed and key-lock)

Playback speed lives in the source-domain `TimeStretchProcessor` slot. The
resampler plan is strictly fixed-ratio: it converts source rate to host rate
only and never carries playback speed.

The whole stretch DSP exists only when a backend is compiled in
(`stretch-signalsmith` / `stretch-bungee`, native targets). `create_effects`
builds one of two chains:

- **fixed-rate** (`stretch = None`): no stretch slot; decoder output remains in
  the planned rate domain.
- **speed mode** (`stretch = Some`): when a backend is compiled in, a
  `TimeStretchProcessor` runs before the resampler and reads live
  `StretchControls` (`speed` + `region_plan` + gated `keylock`/`backend`) each
  chunk. Without a compiled-in backend, including wasm, no slot is added and
  playback speed is pinned to 1.0 in PCM output.

**Live speed routing.** `StretchControls` is the single source of truth,
shared (`Arc`) between the consumer/UI and this slot:

| mode | what the stretch slot does |
|---|---|
| key-lock **on** | `set_ratio(1/speed)`, `set_pitch(1.0)` -> tempo moves, pitch held |
| key-lock **off** | `set_ratio(1/speed)`, `set_pitch(speed)` -> vinyl-style speed and pitch |

At speed 1.0 with no region plan the slot is byte-identical passthrough, so
default playback keeps the old no-DSP behavior. Because controls are read each
chunk, **key-lock, backend, and speed all apply live, mid-track, with no
reload.** A live backend change rebuilds the DSP backend; returning to unity
passthrough resets buffered stretch state. Mobile feature sets
(`apple-fused-src` / `android`) default key-lock to on, so rate changes are
pitch-preserving unless the consumer explicitly disables key-lock.

**Backend seam.** `kithara-stretch` is the optional DSP backend crate. It owns `StretchBackend`, `StretchBackendError`, `StretchKind`, `StretchOptions`, `build_backend`, and the backend adapters behind `stretch-signalsmith` / `stretch-bungee`; `kithara-audio` only enables that dependency through its matching stretch features. `TimeStretchProcessor` owns one `Box<dyn StretchBackend>` selected from `controls.backend()` and rebuilt in place on a live backend swap (or a source-spec change); the trait is DSP-only (interleaved `process`/`flush`/`set_ratio`/`set_pitch`/`max_output_samples`/`reset`) so all `PcmChunk`/pool/timeline plumbing lives in kithara-audio and each library is a small adapter. `set_ratio` is the time factor (`output/input`, >1 = slower); `set_pitch` is independent (1.0 = pitch locked) — that decoupling is what makes key-lock real. Backends select statically via `StretchKind` + `cfg`:

- `signalsmith-stretch` (C++ FFI) — native-only, feature `stretch-signalsmith`.
- `bungee` (C++ FFI) — native-only, feature `stretch-bungee`.

`StretchKind::all()` lists exactly the backends compiled into the current target (the default is `all()[0]`, discriminants are stable: 1 = Signalsmith, 2 = Bungee), so the UI selector never offers an absent one — selecting an uncompiled backend is un-representable, not a runtime error. With no `stretch-*` feature the optional `kithara-stretch` dependency is not linked and the kind/backend/processor re-exports are compiled out; `StretchControls` still exposes speed and optional region-plan storage.

**Region plan (beat-aligned stretch).** The pure region types (`GridSegment`, `RegionPlan`, `RegionPlanError`, and `ActiveRegion`) live in `kithara-audio::region`; public callers use the unconditional `kithara_audio::{GridSegment, RegionPlan, RegionPlanError}` re-exports. `StretchControls` and `TimeStretchProcessor` stay in kithara-audio: controls optionally carry a `RegionPlan` (`ArcSwapOption`, installed via `set_region_plan`, read each chunk - the same live-swap shape as the other controls), and the processor applies the plan. Plans are sorted, non-overlapping `[start_frame, end_frame)` segments in **source frames** (`PcmMeta.frame_offset` space, never output time), each with a `ratio_correction` (validated at construction with a typed `RegionPlanError`). The processor maps each chunk's `frame_offset` to its segment (cached cursor, binary search on a miss after seek/swap), splits chunks at segment boundaries, and drives the backend at `1/speed * ratio_correction`. The backend is `flush`ed (tail drained at the old ratio) + `reset` **only** when a boundary actually moves the effective ratio beyond `RATIO_EPS`; equal-ratio boundaries and gaps between segments (correction `1.0`) cost nothing, and live speed moves inside one region glide via `set_ratio` without a reset. An empty or absent plan is exactly the planless path. For region work prefer `signalsmith` (`bungee`'s no-op `flush` drops the tail at every real ratio boundary).

**Timeline.** A stretch changes the output frame *count*, not the rate: each emitted chunk recomputes `meta.frames` but preserves `timestamp`/`end_timestamp`/`spec` verbatim, so the playhead stays in source-track time - a 3-minute track reads `0:00->3:00` even at 50 % tempo. `bungee` has no clean tail drain through its high-level `Stream`, so its `flush` is a no-op (the final ~latency of audio is dropped at EOS rather than padded with stretched silence). If `bungee`'s `Stream::new` ever fails at construction (only on an invalid spec - unreachable for real stereo/mono audio), the backend warns once and then emits silence until the track is reloaded, rather than erroring per chunk.

## Engine load (live cost meter)

The audio worker measures its own processing cost and publishes it to a shared `EngineLoad` (lock-free `portable_atomic::AtomicF32`s — safe on the forbid-blocking produce core, no allocation). Each time `DecoderNode::tick` produces a chunk it times the whole decode→effects step (`source.step_track`: decode + resampler + EQ + time-stretch) with `Instant` and divides by the produced audio duration (`frames / sample_rate`), EWMA-smoothed. `EngineLoadSnapshot` exposes:

- `realtime` — produced audio-seconds per CPU-second (`>1` = faster than realtime); the cost-per-sample metric for comparing stretch backends live.
- `load` — the `busy / audio` fraction (`0.05` = 5%).
- `ms` — wall time per produced chunk (latency of one processing cycle).

One `Arc<EngineLoad>` is created in `PlayerImpl`, threaded through `AudioConfig::engine_load` → `TrackRegistration` into every track's `DecoderNode`, and read back via `PlayerImpl::engine_load()` → `Queue::engine_load()` for the DJ-studio status bar. The atomics double as the EWMA state; the worker thread is the only writer, so the read-blend-store needs no lock. It reflects whichever track is currently producing and is meaningful whenever audio is flowing — independent of key-lock. `realtime == 0` means "no measurement yet" (paused / not started).

## Construction reads (initial decoder)

`Audio::new` builds the initial decoder **exactly once** (`create_initial_decoder` → one `spawn_blocking`), with **no retry loop and no readiness gate**. The construction read goes through the **blocking** off-RT `Stream::read` adapter — `SharedStream` carries a construction-phase `blocking` flag that `Audio::new` arms before the build and disarms before the RT worker is registered, so the decode loop the worker then drives always uses the non-blocking `probe_read`. (This is a staged construction → steady-state ownership transfer of the read mode, never a live toggle.)

The blocking adapter is what makes construction wait — off the RT worker — for the bytes the build reads (the container header in the init segment, spilling into the first packet). A slow-but-arriving prefix waits up to the stream's blocking-read budget and the build then succeeds, instead of erroring on the first not-ready probe (the old `Audio::new -> Interrupted` flake under load); the wait lives in the stream/byte layer (`Stream::read` wakes the peer downloader), not in a retry loop here. A construction-range byte that genuinely never arrives surfaces the **stream layer's** typed terminal (`Stream::read` → source `io::Error` / typed `StreamPending`) verbatim, bounded by the blocking-read budget — the audio layer never mints its own construction error type, and there is no synthetic `TimedOut`.

A `VariantChange`/`SeekPending` at construction is **not** a rebuild trigger: the variant is settled before the build, and construction never calls `clear_variant_fence` (that stays a recreate-path delegate). Construction always probes at offset 0; a concurrent user seek (play-then-seek) is applied by the post-construction seek path. A `VariantChange` genuinely surfacing at construction would be a stream-state bug to fix in the stream layer, not papered over with a loop. Pinned by `tests/tests/kithara_hls/probe_not_ready_at_creation.rs`.

## Format Change Handling

On a fenced cross-codec ABR variant switch, the `DecoderNode` detects the format change via `Source::media_info()` polling and then:

1. Uses the variant fence on the source to prevent cross-variant reads.
2. Seeks to the first segment of the new variant (where init data lives).
3. Recreates the decoder via `DecoderFactory`.
4. Resets the effects chain to avoid audio artifacts.

Known same-codec HLS switches are not decoder format changes. The HLS source retargets byte mapping at the segment boundary and keeps byte-continuity for the existing decoder; the audio layer must not turn a variant-index-only change into a recreate/fence.

### Decoder recreate policy

- Decoder is **not** recreated on every seek.
- Decoder is recreated when a stream format changes (codec/container boundary) or when post-seek decode reports a recoverable format mismatch.
- Recreate path is metadata-first (`MediaInfo`) with native Symphonia probe fallback from a fresh source.
- Decoder recreate always uses seek target anchor/base offset from timeline/source, so new decoder starts from stream timeline truth.
- **Seek-epoch suppression**: `detect_format_change` returns `NoChange` while a seek is pending and the session was installed at that same seek epoch. The decoder is already aligned with the seek's landing variant, so a second cross-variant recreate inside one seek epoch is wasted work and would discard the freshly-built decoder before it emits a sample.
- **Rebuild supersession retains seek ownership**: when a variant fence makes an in-flight decoder rebuild stale, a newer seek epoch wins first; otherwise a `RecreateNext::Seek` / `ApplySeek` request carried by that rebuild returns to `SeekRequested` and re-resolves against the now-current variant. Only a decode-only rebuild may continue through a fresh `FormatBoundary` recreate. Dropping the carried request leaves the seek flag pending after its one-shot preemption latch was consumed, permanently starving the producer.
- **Seek anchor ownership**: a `SourceSeekAnchor` byte offset is valid only in the variant byte space that resolved it. `ApplyingSeek` re-resolves the seek when the active ABR variant no longer matches the anchor's `variant_index` before `decoder.seek()` runs; `AwaitingResume` may gate on the anchor offset only while the current ABR variant still matches it. After a manual switch or boundary commit changes the active variant, the old byte offset is discarded. This prevents a stale same-codec seek anchor from holding the worker on bytes that are meaningful only in the previous variant layout.
- **Mid-playback recreate continues from the decode head, not `committed`**: a `FormatBoundary` recreate that fires during sustained playback (a variant switch taking effect at a segment boundary, no pending user seek) does **not** bump the seek epoch and does **not** flush the outlet ring. The producer has already emitted chunks ahead of the consumer's lagging `committed_position`, up to its decode head (`source.rs::decode_head`); re-seeking the rebuilt decoder to `committed` would re-emit the `[committed..decode_head]` range that is still queued in the ring — duplicated content the consumer reads as a backward phase jump (the variant-switch phase-drift seam). `execute_recreation` therefore resumes at the decode head. A pending user seek (`resume_target` for the current epoch) takes precedence only while it has not yet materialized in produced chunks (`target > decode_head`): once the producer has trimmed past the seek target, `[target..decode_head]` is already queued in the ring and resuming at `target` would re-emit it — the same duplicated-content seam, gated on CPU contention (the consumer's `committed` lags behind `target` while the producer is already past it; comparing `target` against `committed` mislabeled exactly that case). The decode head is stored as an exact frame and converted to a seek target with `duration_for_frames`; the demuxer quantizes that landing to a sample, and the rebuilt decoder relabels its first chunk via `frame_offset_for`, which rounds the landing PTS to the nearest frame (consistent with `frames_to_trim`). A floored `frame_offset_for` disagreed with that round by one frame when the landing fell a fraction of a sample below the target, leaving a residual −1-sample label seam (the audio stayed continuous); rounding to nearest closes it.
- **Aligned-fence forced recreate (switch-back-to-current race)**: `HlsCoord::commit_variant_switch` raises the read fence only for decoder-recreate switches and publishes the fence's **target variant** (`VariantControl::variant_change_target`) before the generation bump. When the fence targets the variant the session already labels itself with (a seek recreate landed on the switch target before the commit raised the fence — typical for a manual switch back to the variant a racing seek just anchored to), no format diff will ever become observable, and the fence blocks the very reads that drive the seek/recreate paths that clear fences — without intervention the producer would recheck `NoChange` forever and the consumer starves into the watchdog (the pre-recheck code killed the producer with `InvalidData` in the same state). `handle_variant_change` therefore detects this shape (fence target == session variant == published `media_info` variant, no pending seek, init range resolvable) and forces the `FormatBoundary` recreate instead of bare-acking the fence: the session label can lie about the demuxer's actual bitstream (a stale seek anchor stamps the pre-commit variant), so the recreate re-primes the demuxer on the active variant's real bytes, clears the fence inside `apply_format_change`, and resumes from the decode head. A transient pre-publish observation (fence bumped before `abr.apply_decision`) fails the published-`media_info` check and falls through to the bounded recheck.

### Recreate readiness gating

What *kind* of bytes gate a decoder recreate is not guessed by the gate: `recreate_input` (`source.rs`) asks `DecoderFactory::input_requirement` for the demuxer's input contract (kithara-decode `CONTEXT.md` "Decoder input contract"). The contract names the input **shape**; this layer resolves it to a virtual byte range in `recreate_ready_range`, shared by the gate (`source_ready_for_recreate`) and the wait path (`wait_for_source_on_recreate`) so the two never disagree — a mismatch livelocks the worker (the HE-AAC v2 variant-switch hang). The demuxer cannot name the *virtual* range itself, because only the stream knows the ABR byte shift (`served_from`).

- **`InputRequirement::InitOnly`** (segment-aware fMP4): gate on the init header resolved in virtual byte space (`format_change_segment_range`, `served_from`-aware), falling back to the `[offset..offset + DEFAULT_READ_AHEAD_BYTES)` read-ahead window when the init is unaddressable or larger than `DEFAULT_READ_AHEAD_BYTES`. The landing media segment is read by the rebuilt demuxer's first `next_frame`, not gated up front.
- **`InputRequirement::Incremental`** (raw WAV/MP3/FLAC/Ogg): no init to wait for — gate on the read-ahead window directly.

### Playback readiness gating

Steady-state forward decode has the same gate-vs-read contract as recreation: the readiness gate must cover what the decoder actually reads, or the worker hot-spins. `source_is_ready` (the `Track<Decoding>::step` entry gate) clamps its look-ahead window to the **next segment boundary** — it only requires the current segment to be ready before entering `decode_one_step`. But the decoder's container parser reads *across* that boundary into the next segment. When the next segment's body is withheld (slow network), the decoder returns `Pending` while `source_is_ready` still reports `Ready` (the current segment is fully cached). Returning a bare `TrackStep::Blocked(Waiting)` and staying in `Decoding` then re-runs the full `decode_one_step` on every scheduler wake — a busy-spin (`step_track took too long — starving other tracks`), driven hard by the blocking consumer's `recv_outcome_blocking` wake loop (flake F5).

So a `DecodeStep::NotReady` parks in `WaitContext::Playback`, and that wait context's phase (`source_phase_for_wait_context`) gates on `source_phase_forward` — the unclamped `[pos, pos + DEFAULT_READ_AHEAD_BYTES)` window the decoder reads through, **not** the single-byte `phase()` at `pos`. The worker then re-checks that wider window cheaply on each wake and only re-enters `Decoding` (re-running the decode) once it is ready. Gate (`source_phase_forward`) and the decoder's real read never disagree, so the loop parks instead of spinning.

### Source-readiness parks re-aim the producer

A worker parking on source bytes must *also* nudge the producer (the HLS peer) to fetch them, or the peer — aimed elsewhere after a seek/switch — never schedules them and the park never ends (a recreate parks *before* the decoder exists, so unlike the read paths it never reads to trigger the wake itself). `StreamAudioSource::source_park` is the single helper that turns a not-ready `SourcePhase` into a parked `TrackStep::Blocked`, arming the peer wake on every not-ready source, so **every** readiness gate (recreate, playback, post-seek, apply-seek, seek-request) re-aims the producer by construction. Under **flash** the 10 ms scheduler-poll backstop is virtual and collapses, so a missing re-aim surfaces deterministically; in real time the poll masked it.

### Seek error recovery

A failed `decoder.seek()` routes through one shared recovery path that splits by `DecodeError` variant (not string match):

- **Decoder internal-state corruption** (e.g. Symphonia's moof fragment table holding stale offsets after a variant switch) — fresh decoder state resolves this, so recreate is the right cure. This is the default class for any error that is not a typed out-of-range.
- **Caller-side invalid target** (`DecodeError::SeekOutOfRange`: seek past EOF, or a target timestamp outside the stream's known duration) — recreate cannot fix this, since a freshly built decoder has the same `duration()` and rejects the same target. Retrying loops forever (the "seek does not work" prod bug), so these route directly to `fail_seek` with no recreate and no retry.

Init-bearing containers (fMP4/MP4/WAV/MKV/CAF) must recreate at the source's init segment range; a mid-segment recreate would land on bytes with no ftyp/RIFF/EBML header and the factory would fail silently. Mid-stream-decodable containers (MPEG-ES/ADTS/FLAC/Ogg/MPEG-TS) and unknown containers recreate at the offset directly. The recovery path also calls `fail_seek` on missing `MediaInfo` or when an init-bearing container has no available init range.

## Epoch-Based Seek

On seek, epoch is incremented atomically. The worker tags each decoded chunk with the current epoch. The consumer discards stale chunks (old epoch), preventing leftover data from reaching output after a seek.

There are two distinct epoch atomics: the **seek-state** epoch, bumped by the consumer the instant it requests a seek (`Audio::seek` -> `SeekControl::begin`), and the **producer's decode epoch** (`StreamAudioSource::epoch`), advanced only when the worker actually *applies* a seek. The decode epoch therefore lags the seek-state epoch across the window where a seek has been requested but not yet applied. Decoded chunks are tagged with the decode epoch, and terminal markers (EOF / failure) **must** be tagged the same way — via `AudioWorkerSource::decode_epoch()`, not the live seek-state epoch. A near-end seek can drive the decoder to a genuine EOF in the same window where a newer seek has already bumped the seek-state epoch; stamping the marker with the live seek-state epoch would make the stale end-of-stream pass the consumer's validator and surface as a false `ReadOutcome::Eof` for the newer (in-range) seek. Tagging with the decode epoch lets the validator discard it. This race only manifests under CPU starvation (the seek-bump and the EOF-stamp interleave), so its regression guard tags the marker through a mocked `decode_epoch` that differs from the seek-state epoch.

## Agent guardrails

- **Node Architecture**: A track is represented by a single `Node` implementation (`DecoderNode`), stored in the shared scheduler as `Box<dyn Node>` through `runtime/`.
- **Operators vs Nodes**: Audio effects are implemented as operators (`AudioEffect`) that are called directly within the track's `Node`. We do not use separate `Node`s or ring buffers between effects.
- **Chain order**: The effect chain is `[..pre, Resampler, ..custom]`. With `AudioConfig::stretch` set and a native stretch backend compiled in, a source-domain `TimeStretchProcessor` occupies the `pre` slot before the fixed-ratio host resampler and owns all playback speed changes. Without a stretch backend, including wasm, no speed DSP is inserted and PCM output is pinned to 1.0 speed.
- **EOF drain**: At true EOF `StreamAudioSource` drains the effect chain incrementally, one emitted chunk per FSM step: each stage is flushed to exhaustion only after the upstream stage's outputs pass through it, so a buffering effect's multi-pull tail is never truncated and the producer core does not allocate a drain queue. `EndOfStream` fires once on completion, not at source exhaustion.

### Coordinate spaces

The whole pipeline runs in one space: **decoder/song time** (`PcmMeta.timestamp` / `end_timestamp` / `frame_offset`, `Timeline`, `total_duration`, the seek target, the UI playhead). A duration-changing `AudioEffect` changes frame counts but is the sole timeline authority: it restamps only `spec`+`frames` and carries the consumed input's song-time meta forward. The consumer keeps reading `end_timestamp` for position, so no translation layer and no parallel frame counter exist (single source of truth). Reporting perceived/elapsed output time instead would need an explicit translation owner.
- **Buffers**: If a backpressure boundary or rate-matching is needed (e.g. between the worker and the audio callback), a separate buffer `Node` should be introduced explicitly.
- **Push with Backpressure**: Producer nodes call `Outlet::try_push` directly. The outlet has a built-in single-slot overflow that absorbs one backpressure miss per tick, so a producer that emits at most one chunk per tick treats `try_push` as infallible. Each tick begins with `Outlet::flush()` to forward the parked item to the ring once the consumer drains it; if the ring is still full the node returns `TickResult::Waiting`. Under normal operation, the source's FSM is ticked every pass. However, if the outlet is completely saturated (both the ring buffer and the overflow slot are full), the node will return `Waiting` immediately without ticking the FSM. This provides strict backpressure, pausing all internal state transitions (including seeks) until the consumer drains the ring.
- **Cancellation**: Playback nodes do not use `CancelToken`; unregistering a
  track drives `Node::on_cancel()`. The long-lived `AnalysisNode` is the
  deliberate non-playback exception: each queued job owns its scoped token,
  checked before every decode/finalize tick, while scheduler shutdown still
  drives `on_cancel()` for the node itself.
- **Track lifecycle**: A `Node` returns `TickResult::Done` only when truly terminal (e.g. `TrackStep::Failed`). EOF is *not* terminal — the track stays alive so a subsequent seek can re-arm it; idle ticks just return `TickResult::Waiting`.
- `kithara-audio` owns decoder lifecycle, seek or session state, effects reset timing, and stale chunk invalidation.
- Prefer explicit FSM or session objects for multi-step control flow. Avoid scattering new `pending_*` or shadow flags across worker, source, and consumer layers.
- Audio should consume source contracts, not reconstruct HLS or file policy from protocol-specific heuristics.
