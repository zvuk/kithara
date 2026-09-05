# kithara-audio — Context

Contracts the code shape cannot carry. `README.md` owns the overview and type
inventory; `Cargo.toml` owns the feature list.

## Threads and transports

Four contexts touch one track.

- **Consumer** — `Audio<S>`, normally the host audio callback: never allocates,
  frees, or locks.
- **Playback worker** — one shared OS thread and scheduler owned by
  `kithara_play::PlayWorker`, which also owns `DecoderNode` and final producer
  admission. This crate ends at the prepared-lane seam below.
  `kithara-analysis` runs its own single-node runner and shares none of it.
- **Off-RT rebuild** — `spawn_blocking_on` the tokio handle captured at
  preparation.
- **Downloader** — owned by `kithara-stream`; never spawned here, and its
  HLS/file protocol policy is never rebuilt here.

### Cross-crate ownership

Do not re-derive these; each is owned by the named crate's `CONTEXT.md`.

- [`kithara-warp`](../kithara-warp/CONTEXT.md) owns beat maps, session
  coordinates, and the synchronization protocol. This crate publishes decoded
  signal facts only: it never advances a `WarpMap`, reports map progress, or
  acknowledges rendered/presented synchronization.
- [`kithara-signal`](../kithara-signal/CONTEXT.md) owns decoded-signal values and
  pure sample/time math; [`kithara-stream`](../kithara-stream/CONTEXT.md) owns
  encoded/container media types; [`kithara-bufpool`](../kithara-bufpool/CONTEXT.md)
  owns pooled sample storage. This crate only transports those values: it mirrors
  none of their fields and re-exports none of them through a decoder-specific
  compatibility layer. Startup allocation belongs to the composition-root pool
  config, not to an audio pre-warm phase.
- [`kithara-play`](../kithara-play/CONTEXT.md) owns playback cancellation,
  scheduling policy, and playback effects with their reset/drain policy; audio
  sources observe only their scoped cancellation and wake contracts.

### Ring transport

- **Backpressure.** A tick whose SPSC ring and one-slot overflow are both full
  returns *without* ticking the FSM — every internal transition, seeks included,
  pauses until the consumer drains.
- **Wake.** The produce core never enters the kernel; a ring push only arms a
  coalesced atomic. The scheduler shell delivers the pending `ThreadWake` after a
  node visit and before reporting or removing the slot, and an unregistered or
  cancelled node still gets one final shell-side flush, so EOF and failure output
  cannot strand a blocked reader. The consumer snapshots `ThreadWake` before
  re-checking the ring; a signal between snapshot and park advances the gate
  sequence, so the park returns immediately while keeping its timed backstop. A
  seek-epoch drain coalesces every discarded entry into one wake.
- **Trash ring.** The RT consumer must never `free`, so spent pooled
  `AudioChunk`s go to a second ring drained off-RT. Its extra capacity absorbs a
  full forward-ring seek drain, making the RT push infallible.
- **Off-RT deferral.** Signals the forbid-blocking core must not make are armed
  on-core and flushed by the shell: `prepare_deferred` runs before the play-owned
  effect service and resolves source format/state plus build completions;
  `finish_deferred` runs after and owns lifecycle events, the reader→peer wake,
  reader-demand filing, rebuild submission, and `Retired::drain`. Drop keeps one
  teardown flush after the scheduler's final pass, because terminal-slot removal
  performs no further pass. A reader-born event on the RT path also arms the
  worker so that the shell cannot leave that event parked in the deferred bus.

### Preload gate

`PreloadGate` is the one-time startup signal releasing the async consumer's
`preload().await`; its own docs own the lock-free mechanism. Not local to it: the
play-owned `DecoderNode` opens the gate at *every* preload terminal site —
chunk threshold with an empty overflow slot, EOF, `Failed`, cancellation — from
its cached runtime epoch, and re-arms it on seek so a post-seek wait blocks until
that epoch refills.

### Consumer wake capability

`block_on_underrun` is the independent empty-read policy; `ConsumerWakeMode`
names the consumer's thread capability. Two consequences live in neither type:

- Both event routes stamp a monotonic `seq`, but the deferred ring's FIFO is not
  a cross-route delivery order: an inline `ImmediateOffRt` reader event may reach
  subscribers before an earlier-`seq` deferred event still parked in the ring.
- A blocking read parks its caller, so that consumer needs a dedicated thread or
  `spawn_blocking` — never the audio callback, and never a tokio runtime thread
  whose tasks feed the ring. On wasm32 reads never block.

## Ownership map

`StreamAudioSource<T>` is a thin coordinator: it dispatches and is the sole
mutator of track state through `update_state`. Sub-owners never borrow it
mutably; they take disjoint context borrows and return decision values for the
coordinator to apply.

- `SharedStream<T>` — byte-space ground truth; no other owner clones byte-range
  policy. **RT lock discipline:** off-RT holders hold the control mutex across
  waits, so a contended acquire on the forbid-blocking produce core is an RTSan
  violation (`sched_yield` in `parking_lot`'s contended path). RT byte-space
  calls therefore answer from the narrow `SourceProbe`, or from handles resolved
  once at open. Three still lock on RT frames — `probe_read`,
  `media_info`, `seek_time_anchor` — safe only because the sole off-RT holder
  that parks under the mutex is the replacement rebuild reader, which exists
  exclusively while the FSM sits in `RebuildingDecoder`, a phase whose tick
  touches none of the three. That window is also the only time the off-RT reader
  moves the byte cursor, keeping `probe_seek`'s non-atomic sequence
  single-writer.
- `ReadinessGate` — the only owner of byte-range readiness; gate and wait paths
  resolve the same range for the same phase.
- `SeekEngine` — the only writer of the producer decode epoch. In
  `ResumeCursor`, the raw-decode head owns ABR cuts and the rendered-source head
  resolves recreate positions after final output admission; that owner also
  detects route changes.
- `Retired` — off-RT drain for everything the produce core displaces but must not
  free: generations a rebuild replaced, chunks a seek flushed out of staging and
  the gapless buffers. On overflow it leaks rather than freeing on-core.

## Track FSM

`AtEof` has exactly two owners, both semantic (PCM/timeline), never byte-space:
the decode path's exhausted finalization and a seek landing at-or-past
`duration`. Byte-space `SourcePhase::Eof` is a readiness answer, not an end of
track — the demuxer may still hold buffered frames past the last byte, as after a
seek into the final segment — so wait states resume into their `WaitContext` on
it exactly like `Ready`. `AtEof` is consequently non-terminal: a later seek
re-arms the track.

`track::dispatch` runs seek preemption and route change before the phase's own
`step`. Preemption is skipped while `RebuildingDecoder` (recorded as a
supersession instead); route change is skipped in recreate, rebuild, and terminal
phases.

`update_state` publishes the phase to the shared `Activity` PLAYING flag the
downloader peer's `priority()` reads. Every non-terminal phase keeps it set —
buffering, mid-seek, and rebuild windows are still "listened to" — while `AtEof`
and `Failed` clear it. Pinned by the `playing_for_state` tests in
`pipeline/track/tests/state.rs`.

## Readiness gating

`decode::gate` documents the range helpers, their gate-vs-read symmetry
(violating it hot-spins the worker), and segment-boundary clamping. Not local:

- The *shape* of the bytes a rebuild needs is never guessed. The gate asks
  `DecoderFactory::reader_profile` for the demuxer's `ReaderProfile`, and only
  the stream can resolve that named shape into a virtual byte range, because it
  owns the ABR byte shift. The factory first overlays the caller-configured
  `MediaInfo`, so a user declaration selects the profile, not the playlist
  guess.
- The landing media segment is read by the rebuilt demuxer's first frame, never
  gated up front.
- **Source-readiness parks must re-aim the producer**, or a peer aimed elsewhere
  after a seek or switch never fetches the bytes and the park never ends.
  `ReadinessGate::source_park` is the single helper turning a not-ready
  `SourcePhase` into a park with the peer wake armed, so every gate re-aims by
  construction — including a rebuild, which parks before its decoder exists and
  so can never trigger the wake by reading.

## Decoder rebuild

Recreation is two-phase and never builds a decoder on the produce core. On-core,
`RebuildPort::prepare` `probe_seek`s to the recreate offset and stores a pending
job — only one may be pending at a time; the shell then spawns it off-RT, where
it constructs a complete `DecoderGeneration` so that installation only moves one.
The worker consumes only a completion matching the current `BuildId`; a late one
is retired as stale. A matching replacement first aborts any exact incoming
transition, because that transition's landing and prepared blend belong to the
generation being replaced. A caught factory panic becomes
`RecreateOutcome::SoftFailed` and fails the track with
`TrackFailure::RecreateFailed`; only `ErrorClass::Interrupted` maps to
`NeedsSourceWait` and parks.

### Recreate policy

- The decoder is **not** recreated on every seek — only on a real format
  boundary (`variant_boundary`), a host-rate route change, or a non-interrupted
  seek failure. A known same-codec switch in a self-framing container is **not** a
  format change (the source retargets byte mapping at the segment boundary), so a
  variant-index-only change must never become a recreate.
- Init-bearing containers must recreate at the source's init range, never
  mid-segment (no container header there); mid-stream-decodable containers
  recreate at the offset directly. `recreate_offset` encodes exactly this.
- **Seek-epoch suppression**: no recreate is detected while a seek is pending and
  the active generation was installed at that same seek epoch.
- **Supersession retains seek ownership.** When a variant change or newer seek
  epoch makes an in-flight rebuild stale, the recorded `superseded_seek`, else an
  observed newer seek, else the request the rebuild carried, returns to
  `SeekRequested`. Only a decode-only rebuild may continue into a fresh
  `FormatBoundary` recreate; dropping the carried request permanently starves the
  producer.
- **Mid-playback recreate resumes at the rendered source head**, not raw decode
  progress and not `committed`. Such a rebuild bumps no seek epoch and flushes no
  outlet ring, so resuming at the lagging committed position would replay
  already-queued chunks, while raw decode progress may skip source retained by
  Warp or a buffering effect. `Fetch::source_end` carries the decoded-source
  endpoint represented by rendered PCM, committed by the play worker only after
  final producer-port admission; `resume_target` wins only while it is ahead of
  that head. Raw decode progress remains the ABR splice/promotion coordinate.
- A **route change** keeps its container and resolves its origin from the
  running generation's `base_offset` — never a seek anchor, which would root an
  init-bearing demuxer on a media byte. Equal-rate notifications recreate
  nothing, and a route change enters the same state machine as the other causes,
  not a lightweight side path.

## Seek

Two epoch atomics. The **seek-state** epoch is bumped by the consumer the instant
it requests a seek; the **producer decode epoch** advances only when the worker
applies one, so it lags across the requested-but-not-applied window. Decoded
chunks *and* terminal markers are tagged with the decode epoch, never the live
seek-state epoch: a genuine EOF reached after a newer seek bumped the seek-state
epoch would otherwise pass the consumer's `EpochValidator` as the new seek's
terminal.

The consumer side splits in two, because the begin half takes locks while some
consumers sit on an audio device callback. **Begin** (`SeekBegin::begin`) does
that locking off the callback; **adopt** (`Audio::sync_seek`) is lock-free and
drains stale fetches inside the RT no-free boundary — stale chunks to the trash
ring, the first fetch at the new epoch staged rather than dropped. Every read
entry point adopts first, so a new epoch reaches the reader without its caller
touching it; `Audio::seek` remains the one-call form off the audio thread.

A `SourceSeekAnchor` byte offset is valid only in the variant byte space that
resolved it, so a seek is re-resolved whenever the active ABR variant no longer
matches it.

**Error recovery** splits by `DecodeError` variant, never by string match.
`SeekOutOfRange` rejects with no recreate and no retry — a fresh decoder would
reject the same target forever; `Interrupted` parks with the peer re-aimed;
anything else recreates. Missing `MediaInfo`, or an init-bearing container with
no available init range, fails the seek.

**Head trim.** A `pending_head_skip` drops the frames between the chunk timestamp
and the seek target exactly once, on the epoch that requested it; a chunk fully
before the target is dropped whole.

## Variant transitions (gapless splice)

A variant switch can also be spliced gap-free by running a second generation to
overlap. The `IncomingDecode` FSM is driven off-RT, and only from `Decoding` or
`WaitingForSource` in `WaitContext::Playback` (the starved reader an urgent
down-switch exists to rescue); seek and recreate phases are excluded, being about
to replace the decoder a promotion would install. Only `Failed` and an intent
first surfacing after `AtEof` latched abort a transition.

- **Landing.** `landing_for` places the incoming at the outgoing's
  `OutgoingFrontier::Exact` frame, translated through each generation's timeline
  origin; its own docs own why neither the audible playhead nor a point ahead of
  the frontier works. The frontier comes only from `ResumeCursor` — neither
  `WaitingForSource` nor the outgoing disposition replaces a known exact landing
  — and `Awaiting` carries no frame, so the source keeps its seek-derived
  target.
- **The cut is latched, once.** The reader plan's promotion frontier is latched
  in `Preparing` and carried through `Building` into the incoming generation, so
  decoder build latency and later `ResumeCursor` movement cannot move it; once
  latched, outgoing publication stops there, and priming only extends the staged
  span. A same-`AudioSpec` active generation decodes on until its bounded
  holdback covers the real outgoing tail, while a cross-spec transition stops
  immediately: the incoming catches one fixed cut instead of chasing an
  equal-rate stream.
- **The raw cut is recorded before playback transforms.** `ResumeCursor` records
  each post-skip, post-gapless chunk before blending and before the decoded
  source crosses into the play-owned Warp/effect lane, so a buffering or
  frame-changing effect cannot move it.
- **Outgoing exhaustion.** A live transition holds the outgoing's EOF instead of
  dying to it: an outgoing that runs out of source is marked source-exhausted,
  not finished, its holdback and staged tail stay promotable,
  `DecoderEvent::TransitionHold` announces the hold once per transition, and EOF
  finalization waits for the transition to promote or fail (the held pass reports
  `Waiting`, so the hang watchdog bounds it). Even with no slot, finalization
  defers one tick so an intent that raced the last chunk still plants its
  incoming before `AtEof` latches. An exhausted outgoing can never satisfy a
  wait-for-outgoing readiness answer, so those degrade to a hard cut at the final
  frontier; a finished — not exhausted — generation uses the unheld EOF drain.
- **Promotion proof** (`promotion_span`) is fail-closed and minted before
  promotion. A same-spec exact transition demands continuous active PCM from the
  rate-converted outgoing cut through the whole join, and continuous incoming PCM
  from its corresponding cut to the same end. `OutgoingDisposition::Abandoned`
  maps priming and proof to `OutgoingFrontier::Unavailable`, an explicit hard
  cut; `WaitingForSource` alone does not. A retained transition still needs real
  outgoing join PCM — except a source-exhausted outgoing, whose join PCM cannot
  exist past the final decode head and degrades the proof to a hard cut there.
  `Awaiting`, a previous active join, a discontinuous span, or a landed-late
  incoming mints no proof.
- **Promotion** trims the incoming to the proven cut and copies exactly the
  proven active range, leaving only an infallible swap. Every displaced or
  aborted generation goes to `Retired`, never dropped on the produce core.
  Seek/reset cancels an active join and retires every staged chunk.

`GaplessBlender` is always on and owns the audible seam. Profile growth and
buffer resizing happen in the shell before `Priming`, so the on-core path only
moves or reuses state. Gap, mixed-spec, malformed, or over-capacity PCM fails the
decode and stays owned for shell retirement.

## Construction reads

The initial decoder is built **exactly once**, with no retry loop and no
readiness gate, and its read goes through the **blocking** off-RT `Stream::read`
adapter. Each `OpenedReader` carries its own `ConstructionGate`, shared only with
that reader's `SharedStream` clone, so a rebuild cannot switch an active decoder
reader into blocking mode (`decoder_readers_have_isolated_construction_gates`).
Builders arm that gate around each off-RT factory call and disarm it after a
normal return, join error, or caught panic.

The gate selects the read mode and nothing else. `SharedStream`'s `Seek` is the
blocking adapter in **both** phases, because a decoder seeks past residual
lateness in steady state too, where a probe seek could only report not-ready to a
caller that can do nothing but ask again; staying off that path is
`OffsetReader`'s own choice, made by naming `probe_seek`. Blocking lets a
slow-but-arriving prefix wait off the RT worker up to the stream's blocking-read
budget instead of erroring on the first not-ready probe. A construction byte that
never arrives surfaces the **stream layer's** typed terminal verbatim: this layer
mints no construction error type and no synthetic timeout.

A `VariantChange`/`SeekPending` at construction is **not** a rebuild trigger: the
variant is settled before the build, construction always probes at offset 0, and
a concurrent play-then-seek is applied by the post-construction seek path — so a
`VariantChange` here is a stream-layer state bug. Pinned by
`tests/tests/kithara_hls/probe_not_ready_at_creation.rs`.

## Prepared producer seam

`Audio::prepare` returns the reader plus a still-concrete decoded source and
`PreparedAudioLane`. The lane carries exactly what its consumer needs to run the
source; no scheduler, `run_loop`, service class, hang-watchdog policy,
`DecoderNode`, Warp renderer, playback effect, stretch processor, or engine-load
meter lives on this side of it.

Decoded output stays in decoder/song coordinates; a discontinuity publishes its
revision and `AudioSpec` so the downstream owner can reset. `kithara-play`
consumes this seam, owns terminal effect drain and final output admission, and is
the only layer that transforms frame count for playback.

## Sample-rate conversion

Sample-rate conversion for playback is decoder-owned; backends belong to
[`kithara-resampler`](../kithara-resampler/CONTEXT.md) and this crate never picks
a portable default. A requested host rate always resolves to a plan — absent
settings fall back to `B::default()`, so asking for a rate is never silently
dropped; the Apple fused placement converts inside the codec with no standalone
backend, and a backend that cannot serve the ratio fails loudly at construction.
Without a host rate there is no plan and the decoder emits source-rate PCM,
leaving route changes to `ResumeCursor`'s rate guards, which recreate nothing at
an unknown or equal rate. Backend choice is a typed config decision, never a
runtime fallback chain. Output capacity is a correctness invariant, not a knob:
the backend reports `output_frames_for_input` in the ceil frame domain and the
adapter sizes buffers from that.

## Agent guardrails

Repo-wide rules live in `AGENTS.md`; these are the crate's own.

- This crate owns decoder lifecycle, seek/session state, discontinuity
  publication, and stale-chunk invalidation. It consumes source contracts and
  must not reconstruct HLS or file policy from protocol-specific heuristics.
- [`kithara-analysis`](../kithara-analysis/CONTEXT.md) consumes the decoded source
  and owns progressive analysis, waveform/beat artifacts, and analysis bytes; it
  moves no decoder, playback, or cache I/O policy into this crate.
- Prefer explicit FSM or session objects for multi-step control flow; do not
  scatter new `pending_*` or shadow flags across source and consumer layers.
