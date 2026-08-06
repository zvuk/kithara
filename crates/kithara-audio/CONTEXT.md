# kithara-audio — Context

Contracts and invariants. The README owns overview, features, and type inventory.

## Threads and transports

Four contexts touch one track. **Consumer thread** — `Audio<S>` (`PcmRead` +
`PcmSession` + `PcmControl`, umbrella `PcmReader`), normally the host audio
callback: never allocates, frees, or locks. **Renderer worker** — one shared OS
thread per `AudioWorkerHandle` running `runtime::Scheduler` over `Box<dyn Node>`
slots; a track is exactly one node (`DecoderNode`), and effects are `AudioEffect`
calls inside the node's step, never separate nodes with rings between them.
**Off-RT rebuild** — `RebuildPort::submit` → `spawn_blocking_on` on the tokio
handle captured in `Audio::new`. **Downloader** — owned by `kithara-stream`; this
crate never spawns it and never reconstructs HLS/file protocol policy.

Transport (`runtime/ports.rs`): SPSC `ringbuf::HeapRb` plus a one-slot overflow
(`Outlet`/`Inlet`).

- **Backpressure.** `Outlet::try_push` parks one item in the overflow slot when
  the ring is full, so a producer emitting ≤1 chunk per tick treats it as
  infallible. Each tick starts with `Outlet::flush()`; if still full the node
  returns `TickResult::Backpressured` *without* ticking the FSM — every internal
  transition, seeks included, pauses until the consumer drains.
- **Wake.** A ring push wakes the consumer; empty→non-empty also fires
  `on_data_available`. The blocking consumer closes the park/push race by
  re-checking `try_pop` after `ThreadWake::register_current` and before parking,
  with a `park_timeout` backstop.
- **Trash ring.** The RT consumer must never `free`, so spent pooled `PcmChunk`s
  go to a second ring drained by `DecoderNode::recycle` on the worker. Capacity
  `pcm_buffer_chunks + 2` absorbs a full forward-ring seek drain, making the RT
  push infallible.
- **Off-RT deferral.** Signals the forbid-blocking core must not make are armed
  on-core and flushed by the shell from `AudioWorkerSource::flush_deferred`: FSM
  lifecycle events (`DeferredBus<Event>`), the reader→peer wake
  (`ReadinessGate::flush_peer_wake`), retired state (`Retired::drain`).
  `StreamAudioSource::drop` flushes once more — `retain` removes a terminal slot
  without another pass.

### Scheduler

`run_loop` is the unchecked shell (cancel/command drain, slot lifecycle,
`Node::recycle`, the park). Only node ticks run through `produce_tick_rt`, which
carries `#[kithara::rtsan_forbid_blocking]`; `RtPolicy::Heavy` nodes tick outside
it via `produce_tick_heavy`. `Node::rt_policy()` defaults to `Rt`; `AnalysisNode`
declares `Heavy`. `Node::warm_up` runs once at registration in the shell, before
any checked tick, to pre-touch `arc_swap`'s per-thread debt node.

Park budgets: `Waiting`/`UpstreamPending`/`Backpressured` re-check after 10 ms,
`Idle` after 100 ms; a `Produced` streak yields cooperatively every 16 passes
instead of parking. Ticks slower than 10 ms raise `SchedulerEvent::SlowTick`.
`TickResult` separates stalls for the hang watchdog: `Waiting` (watchdog ticks),
`Backpressured` (watchdog must **not** tick, or an idle `Audio` handle panics),
`UpstreamPending` (the source's own demand timeout owns the terminal).

### Preload gate

`PreloadGate` is the one-time startup signal releasing the async consumer's
`preload().await`. The worker is a plain OS thread and must never run a
cross-thread task `wake()`: it does a lock-free `signal_epoch(epoch)` (`Release`
stores of `ready_epoch` then `ready`) and the awaiter polls with `Acquire`,
re-arming its own runtime timer (`POLL_INTERVAL` = 2 ms) while closed.
`DecoderNode` opens the gate at every preload terminal site — preload-chunk
threshold with an empty overflow slot, EOF, `Failed`, `on_cancel` — from its
cached runtime epoch, and `rearm()`s it in `sync_seek_epoch` (`Audio::seek`
rearms consumer-side) so a post-seek wait blocks until that epoch refills.

**`block_on_underrun`.** With `AudioConfig::block_on_underrun(true)` a `read()`
on an empty ring PARKS the caller until the worker produces, so the consumer must
live on a thread the async stack does not depend on — audio callback, dedicated
thread, or `spawn_blocking`. Parking on a tokio runtime thread starves the
downloader tasks feeding the ring. On wasm32 reads never block.

## Ownership map

`StreamAudioSource<T>` is a thin coordinator: it dispatches and is the sole
mutator of track state through `update_state`. Sub-owners never take
`&mut StreamAudioSource`; they take disjoint context borrows (`SeekApplyCtx`,
`DecodeCtx`, `RouteCtx`) and return decision values for the coordinator to apply.

- `SharedStream<T>` — byte-space ground truth (position, len, phase, byte map,
  anchors, init range). No other owner clones byte-range policy.
- `ActiveDecode` — the authoritative active `DecoderGeneration`, the optional
  `IncomingDecode`, the always-on `PcmBlender`, the effect chain, the EOF drain.
  Each `DecoderGeneration` owns its decoder facts, base offset, install epoch,
  per-generation `GaplessStage`, and staged chunks.
- `ReadinessGate` — the only owner of byte-range readiness calculations; gate and
  wait paths must resolve the same range for the same phase.
- `SeekEngine` — `resume_target`, and the only writer of the producer decode
  epoch (`commit_decode_epoch`). `ResumeCursor` — `decode_head` plus host/decoder
  sample rates; resolves recreate resume positions and detects route changes.
- `RebuildPort<T>` — the two-phase rebuild boundary: `prepare` produces a pending
  job, `submit` (from `flush_deferred`) spawns it off-RT. The job constructs a
  complete `DecoderGeneration`; installation only moves it.
- `Retired` — off-RT drain for everything the produce core displaces but must not
  free: generations a rebuild replaced (4 entries) and the chunks a seek flushed
  out of staging and the gapless buffers (64). A `PcmChunk` holds a pooled buffer
  and returning one to a full shard deallocates, so `notify_seek` routes them
  through `ChunkSink` (`kithara-decode`). On overflow the queue `mem::forget`s
  rather than freeing on-core, and warns on drain.
- Format and anchor decisions are pure functions in `decode::format` and
  `seek::anchor`.

`RecreateCause::RouteChange` enters the same recreate state machine as
`FormatBoundary` and `VariantSwitch`; it is not a separate lightweight path.

## Track FSM

Of the `CurrentFsm` phases only `Failed` is terminal (`is_terminal`); `AtEof`
stays alive so a later seek can re-arm the track. `track::dispatch` runs three
stages per step: seek preemption (`preempt_target`), skipped while
`RebuildingDecoder`, which records it into `RebuildState::superseded_seek`; route
change (`start_route_change_recreate_if_needed`), skipped in
recreate/rebuild/terminal phases; then the phase's own `step`.

`update_state` publishes the phase to the shared `Activity` PLAYING flag: every
non-terminal phase keeps it set (the downloader peer's `priority()` reads it, and
buffering / mid-seek / rebuild windows are still "listened to"); `AtEof` and
`Failed` clear it, and entering `Failed` enqueues `AudioEvent::TrackFailed`.
`TrackStep::Done` is reserved for real termination: `TrackStep::Failed` maps to
`TickResult::Done`, EOF does not.

## Readiness gating

What *kind* of bytes gate a rebuild is not guessed: `recreate_ready_range` asks
`DecoderFactory::reader_profile(media_info, byte_map)` for the demuxer's
`ReaderProfile` (kithara-decode/kithara-stream contract) and resolves the named
input **shape** into a virtual byte range, which only the stream can do (it owns
the ABR byte shift). The `DecoderFactory` first overlays the caller-configured
`MediaInfo` codec and container, so a user declaration selects the profile, not
the playlist guess. `ReaderInput::Incremental` gates on the read-ahead window
directly; `ReaderInput::InitOnly` gates on the init header in virtual byte space
(`format_change_segment_range`), falling back to
`[offset, offset + profile.read_ahead_bytes())` when the init is unaddressable or
larger than that window. The landing media segment is read by the rebuilt
demuxer's first `next_frame`, not gated up front.

Steady-state gating has the same gate-vs-read contract, or the worker hot-spins.
`DEFAULT_READ_AHEAD_BYTES` is 32 KiB.

- `source_is_ready` (the `Decoding` entry gate) clamps to `chunk_lookahead_range`:
  the read-ahead window truncated at the **next** segment boundary and at source
  length. On a boundary the range may be empty, deliberately: the demuxer then
  drains input buffered from the previous segment.
- The container parser reads *across* that boundary, so `DecodeStep::NotReady`
  parks in `WaitContext::Playback`, whose phase (`source_phase_for_wait_context`)
  uses the **unclamped** `source_phase_forward` window — the range the decoder
  actually reads through.
- A seek landing gates on `seek_landing_end`: the containing segment's end, or
  the standard look-ahead for flat sources, always clamped to source length so
  the gate never waits past EOF. `post_seek_anchor_offset` gates `AwaitingResume`
  on the anchor byte only while the active ABR variant still matches the anchor's
  variant index.

**Source-readiness parks must re-aim the producer**, or a peer aimed elsewhere
after a seek/switch never fetches the bytes and the park never ends.
`ReadinessGate::source_park` is the single helper turning a not-ready
`SourcePhase` into a parked `TrackStep::Blocked` with the peer wake armed, so
every gate re-aims by construction — including a rebuild, which parks before the
decoder exists and cannot trigger the wake by reading.

## Decoder rebuild

Recreation is two-phase and never builds a decoder on the produce core.
`RecreatingDecoder` gates on `source_ready_for_recreate`, then
`RebuildPort::prepare` `probe_seek`s to the recreate offset and stores a pending
job — only one may be pending at a time. `flush_deferred` → `RebuildPort::submit`
spawns it (`spawn_blocking_on`); the job builds the decoder, optionally seeks it
to its landing time, pushes a `DecoderBuildComplete` onto the replacement or
incoming completion queue (capacity 4 each), then wakes the worker.
`RebuildingDecoder` pops the completion matching its `BuildId`; installation is a
`replace_active` plus a retire. A caught factory panic becomes
`RecreateOutcome::SoftFailed`, failing the track with
`TrackFailure::RecreateFailed`; `NeedsSourceWait` parks (`classify` maps only
`ErrorClass::Interrupted` here).

### Recreate policy

- The decoder is **not** recreated on every seek. Only on a real format boundary
  (codec change, or a variant change in an init-bearing container other than WAV
  — `variant_boundary` / `needs_init`), on a host-rate route change, and on
  non-interrupted seek failures. A known same-codec HLS switch in a self-framing
  container is **not** a format change (the source retargets byte mapping at the
  segment boundary), so a variant-index-only change must never become a recreate.
- Init-bearing containers (fMP4/MP4/WAV/MKV/CAF) must recreate at the source's
  init range, never mid-segment (no ftyp/RIFF/EBML header there);
  mid-stream-decodable containers recreate at the offset directly.
  `recreate_offset` encodes exactly this.
- **Seek-epoch suppression**: `detect` returns `None` while a seek is pending and
  the active generation was installed at that same seek epoch.
- **Supersession retains seek ownership**: when a variant change or newer seek
  epoch makes an in-flight rebuild stale (`policy::superseded`), the recorded
  `superseded_seek`, else an observed newer seek, else the request carried by
  `RecreateNext::Seek`/`ApplySeek` returns to `SeekRequested`. Only a decode-only
  rebuild may continue into a fresh `FormatBoundary` recreate; dropping the
  carried request permanently starves the producer.
- **Mid-playback recreate resumes at the decode head, not `committed`.** A
  `FormatBoundary` + `RecreateNext::Decode` rebuild bumps no seek epoch and
  flushes no outlet ring, so resuming at the lagging `committed_position` would
  replay already-queued chunks backwards, so `ResumeCursor::resume_position`
  resumes at the decode head and `resume_target` wins only while
  `target > decode_head`. The decode head is an exact frame plus its rate,
  converted with `duration_for_frames`; the demuxer quantizes the landing to a
  sample and the decoder relabels its first chunk by rounding to the nearest
  frame, consistent with head trimming.
- A **route change** keeps its container and resolves its origin through
  `anchor::recreate_offset` seeded with the running generation's `base_offset` —
  never a seek anchor, which would root an init-bearing demuxer on a media byte.
  Equal-rate notifications recreate nothing.

## Seek

Two epoch atomics: the **seek-state** epoch, bumped by the consumer the instant
it requests a seek (`Audio::seek` → `SeekControl::begin`); and the **producer
decode epoch** (`SeekEngine::epoch`), advanced only when the worker applies a
seek, so it lags across the requested-but-not-applied window. Decoded chunks
*and* terminal markers (EOF / failure) are tagged with the decode epoch via
`AudioWorkerSource::decode_epoch()`, never the live seek-state epoch: a genuine
EOF reached after a newer seek bumped the seek-state epoch would otherwise pass
the consumer's `EpochValidator` as the new seek's terminal.

Consumer side splits in two, because the begin half takes locks and some
consumers sit on an audio device callback. **Begin** — `SeekBegin::begin`,
implemented by `SeekHandle` (`Audio::seek_handle`) — bumps the epoch, marks
pending, publishes `SeekLifecycle`, notifies the peer, rearms the preload gate
and wakes the worker. **Adopt** — `Audio::sync_seek` — runs
`RingConsumer::begin_seek_epoch` when that epoch differs from the ring's, draining
stale fetches inside the RT no-free boundary (stale chunks to the trash ring; the
first fetch at the new epoch is staged, not dropped). Adopting is lock-free, and
every read entry point (`read`, `read_planar`, `next_chunk`, `preload`) does it
first, so a new epoch reaches the reader without its caller touching the reader.

`Audio::seek` remains the one-call form for consumers off the audio thread.
A `SourceSeekAnchor` byte offset
is valid only in the variant byte space that resolved it, so `ApplyingSeek` (and
a wait carrying it) re-resolves the seek when the active ABR variant no longer
matches `anchor.variant_index`.

**Seek error recovery** (`SeekRecovery::resolve`) splits by `DecodeError`
variant, never by string match. `SeekOutOfRange` (past EOF, or outside the known
duration) → `Reject`, no recreate and no retry: a fresh decoder would reject the
same target forever. `Interrupted` → park in `WaitContext::ApplySeek` with the
peer re-aimed. Anything else → recreate. Missing `MediaInfo`, or an init-bearing
container with no available init range, fails the seek.

**Head trim.** A generation may carry a `pending_head_skip` `ResumeState`;
`seek::skip::apply` drops the leading frames between the chunk timestamp and the
seek target once, on the epoch that requested it, and clears the flag. A chunk
fully before the target is dropped whole.

## Variant transitions (gapless splice)

A variant switch can also be spliced gap-free by running a second generation to
overlap. `ActiveDecode::incoming` is an `IncomingDecode` FSM: `Preparing` →
`Building { build }` → `Priming` (or `Failed`).
`StreamAudioSource::progress_variant_transition` drives it from `flush_deferred`
and only from `Decoding`, or `WaitingForSource` in `WaitContext::Playback` (the
starved reader an urgent down-switch exists to rescue). Seek and recreate phases
are excluded — they are about to replace the decoder a promotion would install —
and terminal phases abort the transition.

- **Landing.** `landing_for` places the incoming at the outgoing's
  `OutgoingFrontier::Exact` frame, translated through each generation's timeline
  origin — never the audible playhead (behind the frontier the gap never closes)
  and never past it (the outgoing stops decoding on a full ring, so a stalled
  consumer would wedge the switch). `Awaiting` and `Unavailable` carry no frame;
  the source keeps its own seek-derived target.
- **Priming** is bounded to 8 decode steps per pass and only extends the staged
  span. `IncomingPrime::Advanced` wakes the rebuild runtime; `Ready` means a
  proof exists; EOF before the frontier is `Failed`.
- **Promotion proof** (`overlap_span`): the incoming must cover the frame the
  outgoing is about to emit **and** be no shallower than the outgoing's own
  staged end, which keeps the bar self-calibrating and reachable. Covering the
  instant alone hands over a sliver, then answers `Pending`, and renders silence.
  `incoming_first_time > outgoing_next` is the distinct "landed late" case
  priming can never fix; the transition waits for the outgoing instead.
- **Promotion** trims the staged head to the cut frame, requires remaining
  output, tells the source (`VariantControl::promote_variant`), swaps the
  generation, and joins the blender. `VariantPromotion::Deferred` stops the pass,
  `Stale` discards the local incoming, and every displaced or aborted generation
  goes to `Retired` — never dropped on the produce core.

**`PcmBlender`** is always on and owns the audible seam. `join_active` starts a
`Pending` join when the two generations share a `PcmSpec` and ≥3 frames of
history are recorded; otherwise it hard-replaces state. The join extrapolates the
outgoing tail with a two-tap recurrence fitted over the last 32 frames
(amplitude-bounded by the observed peak) and cross-fades into the incoming over
20 ms, then returns to `Steady`. Ramp counters are `u16`, so the per-frame gain
is an exact `f32::from`.

## Construction reads

`Audio::new` builds the initial decoder **exactly once**
(`create_initial_decoder`, one `spawn_blocking`), with no retry loop and no
readiness gate. The construction read goes through the **blocking** off-RT
`Stream::read` adapter: `SharedStream` carries a `ConstructionGate`
(`set_blocking`) that `Audio::new` arms before the build and disarms before the
RT worker is registered. `RebuildPort` uses the same gate, carried by
`OpenedReader`, around each off-RT factory call — armed before, disarmed after a
normal return *or* a caught panic. Steady-state reads use non-blocking
`Stream::probe_read`; on-core seeks use `probe_seek` (position math only, no
`prime_seek_range` spin on the forbid path). Blocking makes a slow-but-arriving
prefix wait, off the RT worker, up to the stream's blocking-read budget rather
than error on the first not-ready probe. A construction-range byte that never
arrives surfaces the **stream layer's** typed terminal verbatim; the audio layer
mints no construction error type and there is no synthetic `TimedOut`.

A `VariantChange`/`SeekPending` at construction is **not** a rebuild trigger: the
variant is settled before the build, construction always probes at offset 0, and
a concurrent play-then-seek is applied by the post-construction seek path — a
`VariantChange` surfacing here is a stream-layer state bug. Pinned by
`tests/tests/kithara_hls/probe_not_ready_at_creation.rs`.

## Effect chain and coordinate space

`create_effects` builds `[TimeStretchProcessor?, ..custom]`. There is no
resampler stage in the chain — fixed-ratio conversion is decoder-owned — and the
stretch slot exists only when a stretch backend is compiled in and
`AudioConfig::stretch` is set; without one, including wasm, no speed DSP is
inserted and PCM output is pinned to 1.0.

**Coordinate space.** The whole pipeline runs in decoder/song time
(`PcmMeta.timestamp` / `end_timestamp` / `frame_offset`, the seek target, the UI
playhead). A duration-changing `AudioEffect` is the sole timeline authority: it
restamps only `spec` + `frames` and carries the consumed input's song-time meta
forward, so there is no translation layer and no parallel frame counter.

**Sample guard.** `sanitize_sample` (`kithara-decode`) runs on the *input* of
every stage that takes untrusted samples: `IsolatorEq::process_sample` before it
branches, `PeakLimiter::process_planar` before it takes the frame peak. Input is
the only placement covering every path — the EQ's silence and bypass branches
return their sample untouched, its filter branch would latch a `NaN` into the IIR
for the rest of the track, and one infinite peak drives the limiter's
`ceiling / peak` to zero, fading the master bus back in over the release. The
limiter guards each sample rather than the peak, since `f32::max` returns its
non-`NaN` operand. Bit-exact bypass holds for finite samples.

`IsolatorEq`'s crossover is IIR, so its tail decays into the denormal range and
would cost the callback one to two orders of magnitude more arithmetic. Each
biquad section flushes state and returns exact zero once **both** its input and
output fall below `f32::MIN_POSITIVE`; the input half keeps a live signal through
a deep cut from losing its history.

**EOF drain.** At true EOF `EofDrain` drains the chain incrementally, one emitted
chunk per FSM step: each stage is flushed to exhaustion only after the upstream
stage's outputs pass through it, so a buffering effect's multi-pull tail survives
and the produce core allocates no drain queue. `AudioEvent::EndOfStream` fires
once, when the drain completes — not at source exhaustion.

## Sample-rate conversion

Sample-rate conversion for playback is decoder-owned. `AudioConfig.decoder`
carries `AudioDecoderConfig<B>`, whose optional `DecoderResamplerSettings<B>`
holds the concrete `B: kithara_resampler::ResamplerBackend`, its
`ResamplerOptions`, and its `ResamplerQuality` (`High` by default), combined with
`AudioConfig.host_sample_rate` into `DecoderConfig.resampler`. When absent, the
decoder emits source-rate PCM and `recreates_on_host_rate_change` is false, so
route changes recreate nothing. Backend implementations belong to
`kithara-resampler`; this crate never picks a portable default.
`resample-rubato` / `resample-glide` enable backend types; `apple-fused-src`
forwards to `kithara-decode/apple-codec-embedded-resampler`. Selecting a backend
is a typed config decision, never a runtime fallback chain. Output capacity is a
correctness invariant, not a knob: the backend reports `output_frames_for_input`
in the ceil frame domain and the decoder adapter sizes buffers from that.

## Time-stretch (speed and key-lock)

Playback speed lives in the source-domain `TimeStretchProcessor`; the resampler
plan is strictly fixed-ratio (source rate → host rate) and never carries speed.
`StretchControls` is the single source of truth, shared (`Arc`) between the
consumer/UI and the slot, and read **every chunk** — speed, key-lock, backend,
and region plan all apply live, mid-track, with no reload:

| key-lock | slot behaviour |
|---|---|
| on | `set_ratio(1/speed)`, `set_pitch(1.0)` — tempo moves, pitch held |
| off | `set_ratio(1/speed)`, `set_pitch(speed)` — vinyl-style speed and pitch |

Speed is floored at `MIN_SPEED` = 0.05. At speed 1.0 with no region plan the slot
is a byte-identical passthrough and the backend is reset; `flush` is skipped in
that state so its latency buffer is not emitted as trailing zeros. A live backend
change, or a source `PcmSpec` change, rebuilds the backend in place. Key-lock
defaults to **off** (`StretchControls::new`).

**Backend seam.** `kithara-stretch` is the optional DSP backend crate, behind
`stretch-signalsmith` / `stretch-bungee` (native only); it owns `StretchBackend`
and its companion types. The trait is DSP-only (interleaved sample buffers), so
all `PcmChunk`/pool/timeline plumbing stays here. `set_ratio` is the time factor
(`output/input`, >1 = slower) and `set_pitch` is independent (1.0 = pitch
locked) — the decoupling key-lock depends on. `StretchKind::all()` lists exactly
the backends compiled into the current target (default `all()[0]`; discriminants
are stable: 1 = Signalsmith, 2 = Bungee), so an absent backend is
un-representable rather than a runtime error. With no `stretch-*` feature the
dependency is not linked and the kind/backend/processor re-exports compile out;
`StretchControls` still exposes speed and region-plan storage.

**Region plan (beat-aligned stretch).** The pure region types live in
`kithara_audio::region` and are re-exported unconditionally. Plans are sorted,
non-overlapping `[start_frame, end_frame)` segments in **source frames**
(`PcmMeta.frame_offset` space, never output time), each with a positive finite
`ratio_correction`, validated in `RegionPlan::new`. The processor maps each
chunk's `frame_offset` to its region (cached cursor, binary search on a miss
after seek/swap), splits chunks at boundaries, and drives the backend at
`1/speed * ratio_correction`. It `flush`es (tail drained at the old ratio) and
`reset`s **only** when a boundary moves the effective ratio beyond `RATIO_EPS`
(1e-4); equal-ratio boundaries and gaps between segments (correction 1.0) cost
nothing, and live speed moves inside one region glide via `set_ratio` alone. An
empty or absent plan is exactly the planless path. Prefer `signalsmith` for
region work — `bungee`'s `flush` is a no-op and drops the tail at every real
ratio boundary.

**Timeline.** A stretch changes the output frame *count*, not the rate: each
emitted chunk recomputes `meta.frames` and forces `meta.spec` to the live source
spec, but preserves `timestamp`/`end_timestamp` verbatim, so the playhead stays
in source-track time. A non-empty flush chunk must never carry the
`PcmMeta::default()` sentinel spec (0 channels): downstream stages divide by
`spec.channels`.

## Engine load

The worker measures its own cost into a shared `EngineLoad` (lock-free
`portable_atomic::AtomicF32`s — safe on the forbid-blocking core, no allocation).
Each chunk-producing `DecoderNode::tick` times `step_track` against the produced
audio duration, EWMA-smoothed (`LOAD_ALPHA` = 0.2, seeded from the first sample).
`EngineLoadSnapshot` exposes `realtime` (produced audio-seconds per CPU-second,
>1 = faster than realtime), `load` (`busy / audio`), and `ms` (wall time per
chunk); `realtime == 0` means "no measurement yet" (`is_active()` false). The
atomics double as the EWMA state and the worker thread is the only writer, so the
read-blend-store needs no lock. The meter is created by the playback layer,
threaded through `AudioConfig::engine_load` → `TrackRegistration` into every
track's node, and republished as `AudioEvent::EngineLoad` at most every 500 ms
(buffer health at most every 250 ms), reflecting whichever track is producing.

## Track analysis

`analysis/` owns the reusable per-track analysis engine, generic over
`B: ResamplerBackend`. `AnalyzerBuilder<B>` is the public, `Default`-constructed
selector (`with_waveform`, `with_beat` — which requires `B: Default` —
`with_beat_config`, `with_pcm_pool`), and `is_empty()` lets callers skip
scheduling a pass entirely. `TrackAnalyzers` is the crate-private per-track set;
each analyzer is fed every decoded chunk once.
`TrackAnalysis { beat, waveform, source_frames }` is the public artifact,
`source_frames` being the denominator that turns a `BeatGrid` frame into a
fraction.

`BeatAnalysisConfig<B>` carries the implementation-affecting beat tunables and a
standalone resampler backend handle. Defaults: 1024-frame mono resampler blocks,
22 050 Hz detector input, 30-second detector windows with 2 seconds of overlap,
`ResamplerQuality::High`. The analyzer never stores whole-track PCM: it
downmixes/resamples into a bounded detector window, runs the detector as each
window fills, offsets window-relative events, and keeps only raw event times for
final grid cleanup. Beat scratch buffers come from the builder's `PcmPool`.

`AnalysisWorker<B>` / `AnalysisNode<B>` are a public handle over a second
`runtime::Scheduler` named `kithara-analysis` with one long-lived `Idle`/`Heavy`
node (absent on wasm32). Jobs carry caller-owned cancel tokens; `child_token()`
hands out children of the worker's own job scope, so there is one cancel
hierarchy, and the caller keeps at most one job in flight, cancelling the
previous token to preempt. Results arrive on a `watch` channel: waveform first,
then waveform+beat when a beat pass is configured; on failure or cancel the
sender drops without a value. The node owns the job receiver, the task FSM, and
the single `Box<dyn BeatDetector>` taken at construction — detector ownership is
never shared or locked. `Decode` consumes at most one chunk per tick. The
scheduler park is flash-visible and `analyze` wakes it after enqueueing; no
sleep, backoff loop, or poll watcher. `AnalysisObserver` keeps the normal
no-progress watchdog and separately classifies returned heavy ticks against a
120-second budget; a detector call is indivisible, so an over-budget call can
only be reported after it returns.

**Feature seams.** There is no single `analysis` feature. Artifact types are
unconditional, because region/stretch logic and cache keys use them even when a
pass is absent. `analysis-waveform` gates the `realfft` analyzer (and
`with_waveform`), `analysis-beat` gates the beat analyzer path, `beat-nn` is a
detector backend on top of `analysis-beat`. Without `analysis-beat`,
`with_beat()` is a compile-time no-op — `is_empty()` is the runtime signal.

## Waveform

Pure synchronous DSP turning decoded PCM into a `Waveform` for display. No async,
I/O, cancel, or colour types here: band → colour mapping and orchestration belong
to consumer crates. Tunables live in `AnalysisParams`.

- **Source-only invariant**: analysis runs on the decoded SOURCE signal, never
  post-EQ / post-timestretch / post-resample output. Playback-rate and mixer
  transforms remap only the time axis and never re-run analysis.
- **`Bucket { low, mid, high }`** are three independent band heights per bucket,
  each normalized to `[0, 1]` on one shared scale — not a single bar plus a
  colour. The deck paints them as concentric mirrored bars, so the tallest is the
  outer hull. All-zero is silence, renders as nothing, never `NaN`.
- **Normalized-position index**: buckets are indexed by normalized track position
  `[0, 1]`, never wall-clock seconds. `bucketize` is the single home of that
  mapping — bucket `b` folds the raw range `[b*R/N, (b+1)*R/N)` and always returns
  exactly `N` values, filling an empty range with the supplied `empty`.
  `WaveformAnalyzer::finalize` first clamps the request to the number of analysed
  windows, so a short track yields fewer buckets rather than fabricated ones.
- **PCM ↔ frequency boundary**: `WaveformAnalyzer::new` takes the track
  `sample_rate` because band crossovers map to FFT bins via
  `bin_hz = sample_rate / fft_size`. A constant sample rate per track is assumed;
  build the analyzer once the first chunk's `PcmSpec` is known.
- **Reduction**: per Hann window, band energy is summed into low/mid/high (DC bin
  zeroed; windows below `energy_floor` RMS contribute nothing) and each band is
  divided by its bin count — an energy DENSITY, without which the wide mid/high
  bands outweigh the narrow low band by bin count alone. Windows hop by
  `fft_size / 4` (75% overlap); only tracks shorter than one window fall back to
  a single zero-padded window. `finalize` keeps each bucket's loudest window
  (component-wise max), takes `sqrt` to magnitude, applies the per-band
  perceptual `band_gain`, then divides all three by one shared global max —
  shared, not per-band, so the loudness tilt survives.

## Blob codec

Analysis artifacts persisted to the on-disk cache (`Waveform`, `BeatGrid`) share
one versioned little-endian encoding via the crate-internal, domain-agnostic
`blob` module: the `Blob` trait owns the frame (a `u32` `Blob::VERSION` header
then the body), each artifact implements only its body, and decoding requires the
cursor to consume the blob exactly — trailing bytes are corruption.

Each artifact owns its `VERSION`. A mismatch is `BlobError::Version`; a
truncated, mis-sized, or out-of-range body is `BlobError::Corrupt`. Both are cache
misses — the caller re-analyses and overwrites; there is no in-place migration.
Speculative allocation from an untrusted length prefix is capped at
`MAX_PREALLOC`. `BlobError` is the only piece crossing the crate boundary (the
public `TryFrom<&[u8]>` error); `Blob`, `Reader`, `Writer` stay internal. The
composite track-analysis blob (version + config fingerprint + per-artifact
sections) is an app-layer concern owned by `kithara-app`.

## Agent guardrails

- `kithara-audio` owns decoder lifecycle, seek/session state, effect reset timing,
  and stale-chunk invalidation. It consumes source contracts and must not
  reconstruct HLS or file policy from protocol-specific heuristics.
- Playback nodes do not use `CancelToken`; unregistering a track drives
  `Node::on_cancel()`. The long-lived `AnalysisNode` is the deliberate exception:
  each queued job owns its scoped token, checked before every tick, while
  scheduler shutdown still drives `on_cancel()` for the node itself.
- Prefer explicit FSM or session objects for multi-step control flow; do not
  scatter new `pending_*` or shadow flags across worker, source, and consumer
  layers. If a backpressure or rate-matching boundary is ever needed between
  stages, introduce an explicit buffer `Node` — never smuggle one into an effect.
