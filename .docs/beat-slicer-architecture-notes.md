# Beat Slicer — Architecture Notes for the Next Agent

Status: research notes, not an implementation plan. Written while landing the
timestretch cleanup (drop `timestretch-rs`, move `StretchControls` to
`kithara-audio`, background track analysis). Read this before designing the
beat slicer or the shared analysis worker.

## Where the seams are today

- **Stretch DSP seam** — `crates/kithara-audio/src/effects/timestretch/`.
  One pre-resampler slot (`TimeStretchProcessor`) owns a `Box<dyn
  StretchBackend>` (signalsmith / bungee, feature-gated, native-only). The
  trait is DSP-only: `process` / `flush` / `set_ratio` / `set_pitch` /
  `max_output_samples` / `reset`, interleaved `f32`. All `PcmChunk`, pool,
  and timeline plumbing lives in the processor, so backends stay small.
- **Live controls** — `StretchControls` (always compiled): `speed` atomic,
  plus `keylock` + `backend` under the `stretch-*` features. One `Arc` per
  deck flows from the composition root through `PlayerConfig.timestretch`
  into every track's `AudioConfig.stretch`; the chain re-reads it per chunk.
- **Timeline contract** (crate `README.md`, "Coordinate spaces"): the whole
  pipeline runs in **decoder/song time**. A duration-changing effect
  restamps only `spec` + `frames` and carries the consumed input's
  `timestamp` / `end_timestamp` / `frame_offset` forward. The playhead stays
  in source-track time. The beat slicer must keep this invariant — beat
  positions are source-frame positions, not output-frame positions.
- **Flush contract** (`StretchBackend::flush`): one-shot — after the tail is
  drained, further calls append nothing until the next `process`/`reset`.
  `drain_effects` (`worker/traits.rs`) pulls flush in a loop until it yields
  an empty append; a backend that keeps emitting livelocks the worker (this
  was a real bug in the signalsmith adapter, fixed on this branch).
- **Analysis side** — `kithara-audio/src/waveform/` is a synchronous
  push-style analyzer (`push_interleaved` + `finalize(buckets)`), driven from
  the app (`kithara-app/src/{analysis,waveform}.rs`): one in-flight decode at
  a time, background queue over the library, two-tier cache
  (`wave_cache.rs`, versioned `Waveform::to_bytes`).
- **Cost meter** — `EngineLoad` (`worker/load.rs`): three `AtomicF32` EWMA
  cells, **single writer = the worker thread**. The contract is a comment,
  not a type; do not wire the same `Arc` into a second worker.

## Reviewer roadmap (agreed direction)

1. **Shared analysis worker in `kithara-audio` (next PR).** Move the
   app-side runner down so iOS/Android can reuse it. Shape it as a registry:

   ```text
   AnalysisWorker (own thread, one per app/deck)
     ├─ register(Box<dyn TrackAnalyzer>)   // waveform, bpm, pitch, …
     ├─ one decode pass per track          // single Resource/PCM stream
     └─ fan-out: each chunk is pushed to every registered analyzer
   ```

   Key decision: **one decode, many analyzers**. The current waveform path
   opens a full second decode pipeline per analysis; adding bpm/pitch as
   separate passes would multiply that. `WaveformAnalyzer`'s push/finalize
   shape is already the natural `TrackAnalyzer` trait (sync, sample-rate
   parameterized, lazily constructible from the first chunk's spec);
   `bucketize.rs` is reusable for any position-mapped series (bpm/energy).
   `TrackAnalysis` (`kithara-app/src/waveform.rs`) is `#[non_exhaustive]`
   precisely so bpm/beatgrid fields can be added.

2. **Type ownership.** `BeatGrid` / `BpmInfo` currently live in
   `kithara-play::traits::dj::bpm`, but the analyzer that will *produce* them
   belongs to `kithara-audio`. Per `AGENTS.md`, name the canonical owner
   before wiring: either move the data types into `kithara-audio` (analysis
   artifacts, like `Waveform`) and re-export from play, or keep them in play
   and have the audio-side analyzer emit a neutral struct. Do not duplicate.

## Beat slicer: recommendations

The goal: stop stretching the whole track as one stream — split it into
regions (beat-aligned slices) and stretch each region independently.

- **Region = source-frame range.** Represent a region as
  `[start_frame, end_frame)` in decoder/song time (`PcmMeta.frame_offset`
  space). Beat-grid analysis yields the boundaries. Never store region
  bounds in output/stretched time — that breaks the single-coordinate-space
  invariant and every seek.

- **Two viable execution models** — pick per use-case, they compose:
  1. *Streaming region automation* (live deck): keep the existing
     `TimeStretchProcessor` slot, but feed it a region plan. Per chunk the
     processor knows `frame_offset`, so it can detect a boundary crossing
     mid-chunk, split the chunk at the boundary sample, `flush` + `reset` +
     `set_ratio(next)` and continue. The `StretchBackend` trait already
     supports this (reset + ratio change is exactly the live-swap path);
     what's missing is only the split-at-boundary logic in the processor.
  2. *Offline region render* (pads / hot cues / loops): decode a region via
     the analysis-style path (`Resource` + shared stores), stretch it with a
     standalone backend instance, cache the rendered PCM. Reuse the
     versioned-bytes cache pattern from `Waveform::to_bytes` /
     `TrackAnalysisCache` for rendered slices.

- **Region plan as shared state.** Keep the plan in one `Arc`-swapped value
  (the workspace already depends on `arc-swap`), owned by the deck (app/play
  layer), read by the processor per chunk — the same pattern as
  `StretchControls`. Avoid free-floating `pending_region_*` flags; the plan
  is one explicit object.

- **Backend caveats for region boundaries:**
  - `bungee`'s `flush` is deliberately a no-op (no clean tail drain through
    its high-level `Stream`), so a hard region cut drops ~latency of audio.
    For region work prefer signalsmith, or overlap regions by the backend
    latency and crossfade at the seam.
  - Both backends emit leading latency-fill after `reset`; a per-region
    `reset` therefore costs a transient. The streaming model should only
    reset when the *ratio actually changes* at a boundary, not on every
    boundary.
  - `max_output_samples` is the pre-reserve contract — region splitting must
    keep calling it per sub-chunk to stay alloc-free on the produce core.

- **Do not push slicing into shared crates' callers.** The slicer is an
  audio-crate effect concern (region-aware stretch slot) plus an app/play
  concern (region editing, beat-grid UI). `kithara-play` should only carry
  the plan handle, like it carries `StretchControls` today.

## Lessons from this branch (apply to slicer/analysis work)

- **Cancel hierarchy is a non-negotiable.** The analysis path used to build
  an `Audio` with no cancel (root token via `unwrap_or_default()` — outside
  the app tree). Every new worker/resource must take a child token from its
  owner (`analyze_track` now does `config.cancel = Some(cancel.child_token())`).
- **Production builds ship a panicking hang-watchdog.** `kithara-test-utils`
  (feature `hang`, default-on) is a regular dependency of
  file/storage/audio; `HangDetector::tick` panics after ~10 s of
  "no progress". The TmpClaimed wait in `kithara-file/src/stream.rs` used to
  count a *live* sibling download as no-progress — two consumers of one URL
  (player + analyzer) crashed the app on any download longer than the
  timeout, and the leftover `.tmp` crashed the *next* launch too. Fixed by
  ticking the watchdog only when the claimed tmp stops growing. Follow-ups
  worth considering:
  - stale-tmp reclaim in `kithara-storage` (a tmp that does not grow and has
    no live writer should be evicted, not waited on until a panic);
  - the crate claims "no-op in release" but `hang` is feature-, not
    `debug_assertions`-gated — decide the intended production policy once.
- **Single-writer atomics need a named owner.** `EngineLoad::record` is a
  non-atomic read-modify-write; a second writer (e.g. a future analysis
  worker reusing the player's `Arc`) silently corrupts the EWMA. Give the
  analysis worker its own meter if it needs one.
- **`WaveformAnalyzer` drops the final partial window** (`< fft_size` tail)
  unless the track is shorter than one window. Fine for a coarse waveform;
  a bpm analyzer reusing the pattern would lose tail beats — flush the
  partial window there.
- **Waveform cache version coupling.** `WAVEFORM_BYTES_VERSION` must be
  bumped when `AnalysisParams` defaults or the app's `WAVEFORM_BUCKETS`
  change; the coupling is doc-comment-only. The analysis worker should fold
  the parameters into the cache key instead.
