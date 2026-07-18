# 2026-07-15-session-transport-plan

## Goal

Implement the architecture in
`docs/plans/2026-07-15-session-transport-design.md` as vertical, test-driven
slices. The result is a Kithara-owned, session-scoped analytical musical
transport over an unchanged Firewheel graph adapter, explicit track-to-session
binding, source-range planning, multi-track elastic rendering, transactional
tempo and seek changes, per-track reverse, and offline-to-disk acceptance
coverage through the existing integration-test session.

## Success Signal

- [x] One Kithara-owned `SessionTransport` is shared by every player in a
      session.
- [x] Its beat position is derived analytically from a Kithara anchor and
      re-anchors continuously at an exact graph-adapter boundary.
- [x] Track bindings use real analysed beat maps and typed coordinate spaces.
- [x] Every player node receives the same immutable render context for a
      processed graph block.
- [x] Two and then four tracks remain phase-aligned through a tempo change.
- [ ] Seek, reverse, and read-ahead obey explicit source-range and revision
      contracts.
- [x] The integration session renders the real native forward bound path to
      memory and to WAV.
- [x] The same integration session renders the real reverse path to memory and
      to WAV after directional preparation is implemented.
- [ ] Legacy entertainment-role event naming is migrated to transport and
      synchronization vocabulary before new sync events become public.
- [x] Existing forward playback and legacy pitch bend do not regress.
- [ ] The transport and unchanged graph adapter compile and pass their shared
      contract on Android and browser `wasm32-unknown-unknown` surfaces.
- [ ] Repo formatting, lint, and test acceptance gates pass without suppressions
      or baseline growth.

## Status

- [x] Architecture and terminology agreed with the user.
- [x] Latest `production/main` inspected; role-first ownership adopted.
- [x] Firewheel musical transport audited and rejected as the musical authority;
      Kithara-owned transport over an unmodified graph adapter selected.
- [x] Replace the transitional Firewheel musical-transport implementation with
      the Kithara-owned analytical anchor and exact-boundary adapter event.
- [x] Rebase the legacy pitch-bend slice onto `production/main`; drop superseded
      host-time clock and override-only grid commits.
- [x] Slice 1 reopened: replace the completed Firewheel tracer with the
      Kithara-owned session transport while preserving its offline assertions.
- [x] Slice 2: typed beat map, explicit sample-rate axis, and active-track
      binding ownership.
- [x] Slice 3: one immutable render context per processed graph block.
- [x] Slice 4: source audio sidecar, immutable cache, and forward read-ahead.
- [x] Slice 5: one native forward bound track, numeric elastic requests,
      independent off-RT preparation, source-authoritative rendering, and
      rolling source windows.
- [x] Slice 6: multi-track participant readiness and an all-or-nothing tempo
      commit at one presentation boundary.
- [x] Slice 7: transactional session seek and explicit join.
- [x] Slice 8: prepared file-source reverse and directional read-ahead.

## 2026-07-16 Slice 5 Checkpoint

Slice 5 closes one deliberately narrow vertical path: one native,
stream-backed item with a forward `TrackBinding` renders from the shared
session context through a pitch-preserving `ElasticRenderer`. A reopenable
`ResourceBlueprint` creates an isolated preparation reader. The renderer and
its initial source window are allocated, filled, and primed off the audio
callback, then transferred to the active track together with a dedicated
source worker.

Bound preparation reads tempo, transport revision, sample rate, and maximum
block size as one session-owned snapshot. Only a fully prepared item is
published to the playlist. Missing or saturated command lanes reject the load
before source-worker activation and restore its resource, binding, dormant
renderer, and preparation stamp before another playlist mutation can
interleave. On the audio thread, deferred-retirement backpressure retains
ownership and pauses later command draining until retirement succeeds; it does
not drop the renderer or source worker on the callback.

The existing offline integration session exercises this native forward path,
checks that the rendered signal remains pitch-preserved, and writes the same
rendered samples to WAV. Internal preparation readers use an isolated event
scope, so their seek lifecycle does not leak into the public resource bus.
WASM compilation proves the explicit platform boundary only; it is not browser
elastic-playback acceptance.

The current limits are intentional and typed:

- browser WASM bound insertion returns `ElasticBackendUnavailable`;
- adaptive/multi-variant HLS reverse returns `ReverseSourceUnavailable` until
  reverse ABR re-aiming has a protocol-owned discontinuity contract;
- track-local seek on a current bound item returns
  `BoundTrackSeekRequiresSessionTransport` without moving the player position;
- custom readers cannot be reopened for bound preparation and return
  `BindingSourceNotReopenable`.

Slices 10-12 remain required. Slice 10 owns successor prefetch. Slice 11 owns
full offline, real-time, performance, and device/platform acceptance, and Slice
12 owns event vocabulary migration.

Open implementation boundaries retained for the next slices:

- define natural end behavior at the analysed marker domain instead of letting
  `OutsideMarkerDomain` become a track failure;
- replace the wall-clock assumption in faster-than-real-time rolling tests with
  an explicit non-real-time service/yield contract;
- prove the rolling horizon against a declared worker turnaround and cache
  assembly bound before claiming uninterrupted playback under arbitrary load.

### 2026-07-16 Explicit-Wake Decision Log

Implemented and validated: use owner-specific generation signals instead of
time. `SourceAudioActivity` is the decode-side activity owner; `ThreadGate` is
the real-time port-edge wake boundary. A dedicated named platform thread owns
the blocking source worker independently of Tokio. It snapshots generations,
rechecks cancellation and available work, and waits only while the observed
generation remains unchanged. Data, status, producer-close, port-edge,
cancellation, runtime-teardown, and lost-wake contracts have direct tests.

Rejected alternatives:

- Tokio `Notify` from the real-time path: executor/waker machinery does not
  belong on the audio callback boundary.
- Periodic timer polling: it couples progress to wall-clock delay, adds idle
  wakeups, and makes faster-than-real-time offline behavior nondeterministic.

Neither Tokio notification nor timers are permitted on the real-time path.

The approved route remains a Kithara-owned analytical transport plus an
adapter-scheduled real-time commit on the unchanged Firewheel graph. Firewheel
musical transport and dynamic keyframes remain outside the contract. No fork,
private patch, upgrade dependency, or upstream change is allowed.

### 2026-07-18 Player Ownership Recovery

The dedicated Slice 6 participant barrier and the uncommitted Slice 7 control
plane were rejected. They duplicated player-owned membership, preparation,
binding, and queue state across session and callback modules and made the
transport a parallel player implementation.

The existing ownership chain remains canonical:

- `PlayerImpl` and `ItemQueue` own control-side track and playlist state;
- `EngineImpl` and `SlotControl` publish commands to the allocated slot;
- `PlayerNodeProcessor` and `PlayerTrack` own real-time track state;
- `SessionState` owns only the shared analytical transport and graph boundary.

Slices 6 and 7 must extend this path directly. The session may select one shared
render boundary and publish one immutable context, but it must not own a copy of
slot membership or per-track preparation. New player-node tempo owners,
participant registries, processor transactions, track transactions, and
parallel elastic control planes are forbidden.

Rejected alternatives:

- keeping the participant barrier and merely reducing its module count;
- retaining the dirty Slice 7 control plane while moving its files;
- exposing a second public track-control protocol beside `PlayerImpl`.

### 2026-07-18 Slice 6 Checkpoint

Slice 6 extends the canonical player path directly. The caller supplies the
participating `PlayerImpl` values; their existing `ItemQueue` locks are acquired
in `PlayerId` order only for readiness and stamp publication. `SessionState`
atomically verifies the expected transport revision, stream shape, and its
existing active graph roster before scheduling the normal tempo commit. It does
not retain a participant registry, binding mirror, or renderer state.

Native readiness plans the old span through the future boundary and the first
new-tempo span with the same marker-splitting planner used by the renderer. It
checks each current bound track, including paused tracks, against the declared
rate envelope and decoded frontier. Bound preparation is rejected while a
future tempo commit is pending, and a player-local handover rejects explicitly
until it has one current audible binding. Browser players keep the same API and
return the existing typed backend-unavailable result for a bound track.

The offline session covers two and four different source tempos, one shared old
and new revision, beat continuity at the exact boundary, one-output-frame phase
tolerance, WAV output, unsupported rate, incomplete roster, paused peer, and
local-handover rejection. Focused renderer preflight coverage proves
insufficient look-ahead and marker-local rate rejection. Signalsmith renderers
prime and discard their independently declared latency before publication,
leaving one presentation cursor per track without a session-owned latency
mirror.

### 2026-07-18 Slice 7 Checkpoint

Transactional session seek and explicit join both extend the canonical player
path. Seek prepares relocation on each participant's existing
`ElasticRenderer`, publishes through the existing slot lanes, and commits one
checked session transport revision only after every participant is ready.
Failure cancels the prepared relocation and leaves the transport unchanged.

Explicit join adds a bound resource only to an idle `PlayerImpl`. The native
implementation prepares at the requested future beat, holds the existing
`ItemQueue` lock from insertion through command publication, and rolls the
insert back on rejection. `PlayerTrack` stores the pending beat and transport
revision, remains silent before the exact `RenderContext` output offset, and
starts within that callback without mutating session transport. The WASM
implementation is structurally separate and returns the typed elastic backend
capability error. No participant registry, second renderer, session binding
mirror, or alternate real-time control plane is introduced.

### 2026-07-18 Slice 8 Checkpoint

File-source reverse extends the same `PlayerImpl` -> `PlayerTrack` ->
`ElasticRenderer` path used by forward playback. The renderer requests one
bounded ascending source range, primes Signalsmith history in reverse audible
order, and consumes the cached window from high frames to low frames without
wrapping at source start. Direction-aware rolling windows still issue only
ascending range requests.

Prepared renderer state records its direction and rejects a mismatched render
until that history has been prepared. File resources declare reverse source
access explicitly; HLS and custom readers return a typed capability error
before playlist publication. Native source access lives in one native module,
while the existing WASM player implementation remains structurally separate.
The integration session proves reverse marker order through the real file
reader and writes the rendered result to WAV. HLS reverse remains Slice 9.

### 2026-07-18 Slice 9 Checkpoint

Single-variant HLS reverse extends the same `Resource` source-audio lane and
`ElasticRenderer` used by file reverse. HLS remains the owner of segment,
access-point, preroll, cache, and discontinuity resolution. The renderer keeps
all requests ascending and bounded, prepares two small successor windows before
playlist publication, and maintains that fixed-depth inline FIFO while the
active window remains immutable until its overlap boundary.

The integration session uses a two-segment HLS look-ahead cap, crosses every
media boundary in descending audible order, reaches frame zero, and proves that
no stale marker is replayed afterward. Adaptive HLS is rejected with
`ReverseSourceUnavailable` before playlist publication; this is the explicit
variant/discontinuity policy until protocol-owned reverse ABR re-aiming is
proven. Custom readers retain the same typed capability rejection.

The HLS reverse session passed three consecutive isolated runs. The first full
workspace run then exposed a deterministic forward-readiness regression: a
prepared successor window was renderable but absent from `decoded_frontier`.
Publishing the maximum frontier across the active window and ready FIFO fixed
the contract; the focused elastic suite passed 16/16 and the final workspace
harness passed 3803/3803 with 145 skipped. Native lint, formatting, architecture
ratchets, and the structurally separate WASM lane are green.

## Affected Paths

- `Cargo.toml`
- generated workspace dependency configuration
- `crates/kithara-play/src/api`
- `crates/kithara-play/src/session`
- `crates/kithara-play/src/engine`
- `crates/kithara-play/src/player`
- `crates/kithara-play/CONTEXT.md`
- `crates/kithara-audio/src/analysis` and `crates/kithara-audio/src/musical`
  as the analysis and beat-map owners
- `crates/kithara-audio` as the `SourceAudio` and cache contract owner
- `tests/tests/kithara_play`
- offline render/WAV fixtures in the existing integration-test owners

## Required Reads

- `AGENTS.md`
- `docs/workflows/rust-ai.md`
- `docs/guides/test-harness.md`
- `docs/plans/2026-07-15-session-transport-design.md`
- matching `README.md` and `CONTEXT.md` for every slice's owned crates
- Firewheel `0.10.x` render-clock, processor-event, pre-process sub-block, and
  scheduled-event APIs for adapter changes
- Android and browser WebAudio/WASM build and integration owners before the
  platform acceptance gate

## Constraints

- `SessionState` is the sole control-side ledger and revision allocator;
  `TransportCommitState` is the sole real-time analytical anchor owner.
  Revisioned `Stage`/`Apply` events are the only ownership-transfer boundary.
  `FirewheelCtx` owns only the graph, render clock, and event delivery.
- No parallel host-time clock, beat clock, tempo authority, Firewheel musical
  fallback, or hidden fallback.
- Musical state does not cross into stream, HLS, or decode APIs.
- All tracks in a render sub-block observe one immutable transport context.
- New public types require a documented contract and contract tests.
- New items default to `pub(crate)`; public enums and named-field structs follow
  workspace API rules.
- No new lint suppressions, baseline entries, speculative helpers, or unused
  abstractions.
- Source decoding remains forward even when audible direction is reverse.
- Every behavior slice starts with a failing contract test.

## Non-Goals

- Whole-session reverse.
- Session pause/resume transactions in the current tempo slice.
- New beat-analysis algorithms.
- Firewheel musical transport, dynamic keyframes, or transport speed.
- Any Firewheel fork, private patch, or upstream-change dependency.
- Truce integration or graph replacement in this wave.
- A broad rename of existing legacy audio API.
- Shipping offline rendering as a product-facing feature; it is an integration
  and artifact-validation surface for this plan.

## Validation Scope

- During a slice: the smallest owner-package unit or integration test proving
  the red/green contract.
- After each slice: scoped package check, formatting, and relevant integration
  lane through repo wrappers.
- Before reporting a complete wave: `cargo xtask test` or `just test` as the
  acceptance authority, plus repository lint/format gates.
- Offline audio claims include a generated WAV artifact and deterministic
  marker/phase assertions, not merely successful construction.
- Android API 26+ and browser `wasm32-unknown-unknown`/WebAudio are mandatory
  shared-contract gates before this subsystem is complete.
- Apple/device behavior is validated after the shared render contracts are
  green.

## Split Map

- Slice 1 transport agent owns removal of the Firewheel `musical_transport`
  feature from `Cargo.toml` and generated workspace-hack output,
  `crates/kithara-play/src/api/transport.rs`, session transport/protocol/state/
  dispatch files, engine transport facade, matching integration test, and the
  owning `CONTEXT.md`. It must read the design, play README/CONTEXT, and
  Firewheel render-clock/event APIs. It must not modify Firewheel source and is
  forbidden from changing player, queue, stream, HLS, decode, audio-render, or
  unrelated event APIs.
- Slice 2 beat-map ownership is `kithara-audio`; the immutable binding belongs
  to the active `kithara-play::PlayerTrack`. `SessionState` owns neither maps nor
  per-track bindings.
- Slice 4 source-range ownership is `crates/kithara-audio` cache/range files
  selected in its discovery gate. It is forbidden from changing stream, HLS, or
  decode public APIs; any required adapter contract returns to the integrator.
- Slice 5 render paths are selected only after Slice 3 freezes `RenderContext`
  and the latency-map contract. It is forbidden from changing session transport
  ownership.

Agents may work in parallel only after public boundaries are frozen and their
write paths do not overlap. The integrator alone changes shared contracts.

## Integrator

The primary Codex session owns discovery gates, shared types, sequencing,
cross-crate contract changes, diff review, and all acceptance claims. Future
task packets must add exact owned paths, forbidden paths, required reads, and
their dependency on a completed prior slice before split execution begins.

## Sequencing Dependencies

Legacy Wave 0 rebase -> Slice 1 -> Slice 2 -> Slice 3 -> Slice 4 -> Slice 5 ->
Slice 6 -> Slice 7 -> Slice 8 -> Slice 9 -> Slice 10 -> Slice 11 -> Slice 12.

## Slice 0 - Preserve The Valid Legacy Baseline

### Goal

Rebase the existing consume-side pitch-bend implementation onto the role-first
`production/main` layout without making it the session synchronization model.

### Expected Output

- Unity remains bit-transparent.
- Fractional cursor, look-ahead, seam interpolation, seek reset, and bounded
  audible-rate behavior retain focused tests.
- Public player pitch-bend behavior remains wired.
- Superseded host-time session clock and override-only player grid commits are
  skipped rather than mechanically restored.

### Validation

- Owner audio unit tests.
- Focused pitch-bend integration test.
- Package checks for `kithara-audio` and `kithara-play`.

## Slice 1 - Kithara Session Transport Tracer

### Goal

Prove one Kithara analytical transport, driven by graph render frames and shared
by multiple engines in the existing manual offline session.

### Red Test

1. Create one manual offline session at 44.1 kHz.
2. Connect two engines to its dispatcher.
3. Set session tempo to 120 BPM through one engine.
4. Render exactly 22,050 frames.
5. Query through both engines and assert an identical position exactly one beat
   after the initial position within one-sample tolerance.

Follow with tempo validation, 120 to 60 BPM continuity, and block-partition
independence tests.

The reopened migration tracer stages 60 BPM at one adapter-owned future render
boundary inside an offline render. A one-shot render and a partitioned render
must keep the old revision before that frame, apply the new revision exactly at
that frame, and finish at the same analytically derived beat.

### Owned Paths

- removal of the workspace Firewheel `musical_transport` feature and generated
  dependency config update
- `crates/kithara-play/src/api/transport.rs`
- `crates/kithara-play/src/session/transport.rs`
- session protocol/state/dispatch wiring
- engine transport facade
- `tests/tests/kithara_play/session_transport.rs`
- `crates/kithara-play/CONTEXT.md`

### Constraints

- Firewheel musical types do not cross any Kithara boundary.
- Validate finite positive tempo before mutation.
- Derive beats only from the Kithara anchor and adapter render clock; never copy
  or query Firewheel musical transport.
- A late, stale, aborted, or stream-reset scheduled commit fails explicitly; it
  is never silently moved to a different boundary.
- The existing render context and revision types are retained and migrated to
  the Kithara anchor rather than duplicated.
- `SessionState` explicitly owns the Slice 1 transport lifetime. Stopping the
  final engine currently ends that session transport and a later activation
  begins at beat zero; changing idle-session persistence is a separate
  lifecycle decision.

## Slice 2 - Typed Beat Map And Track Binding

### Goal

Map real analysed markers between `TrackBeat` and `SourceFrame`, then bind one
active track anchor to `SessionBeat`.

### Tests

- marker interpolation and inverse mapping;
- no extrapolation beyond analysed markers and no marker beyond source EOF;
- negative phase and non-zero anchors;
- 3/4 and 4/4 fixtures;
- source-rate to host-rate conversion and active-track axis invalidation;
- invalid or absent map returns typed `SyncUnavailable`;
- tempo override alone cannot create a map.

### Constraints

- Immutable analysis data and binding values; the current binding belongs to
  the active track. Binding revision allocation begins with the first
  prepare/commit owner that consumes it, not as an unused constant here.
- No decoder-position polling as a phase authority.

## Slice 3 - One Immutable Render Context

### Goal

Construct one `RenderContext` from adapter render-frame/sample-rate information
and Kithara transport state, then pass it to all tracks rendered in the
sub-block.

### Tests

- two player nodes receive the same stored context address, render-frame range,
  host sample rate, and transport snapshot;
- public standalone nodes retain their context-free render contract while only
  session-created nodes require the shared snapshot;
- inactive transport is a valid context with no session-beat range;
- invalid transport input replaces the previous valid snapshot;
- a node-local sub-block cannot reuse a full-block snapshot;
- no per-track transport query occurs;
- block partitioning preserves the final session position.

### Constraints

- `SessionState` installs one zero-I/O pre-process writer before stream start.
- Context requirement is an explicit construction mode, not a fallback: public
  standalone nodes do not query the store, while session-created nodes fail
  closed when the shared snapshot is unavailable.
- The preallocated `ProcStore` slot delivers the snapshot but does not own a
  clock or mutable transport state.
- Player nodes accept only a snapshot whose signed render range and host sample
  rate exactly match their `ProcInfo`; node-local event sub-blocks fail closed
  until their derivation contract is implemented.
- The full context reaches every normal, handover, and promoted track render;
  resource and lower read APIs remain context-free.

## Slice 4 - Source Audio Cache And Forward Read-Ahead

### Goal

Introduce transport-independent `SourceAudioCache`, explicit source ranges,
and generation-aware demand while preserving forward audio. The audible
directional cursor begins in Slice 5; decoder ingestion never owns it.

### Tests

- cached and uncached forward output match the existing path;
- seek invalidates stale completion without evicting reusable immutable ranges;
- demand includes renderer look-ahead and remains bounded;
- stream/HLS/decode APIs contain no musical context.
- `kithara-audio` owns `SourceAudio`: decoded, gapless-normalized, host-rate
  audio indexed by `SourceFrame`.
- the existing processed-audio ring stays byte-for-byte compatible while an
  opt-in source sidecar is extracted into `Resource`;
- demand identity includes lane, generation, and decoder seek epoch;
- source windows use complete post-normalization metadata ranges; overlapping
  extensions preserve the first cached samples and add only newly reachable
  suffix/prefix coverage;
- capture buffers grow to an observed decoder window only in the worker shell,
  then the checked path retries the same epoch-tagged chunk;
- activation, demand, data, and retirement use distinct bounded ports;
- the decode-side write head is not the audible `DirectionalCursor`; the latter
  belongs to the later elastic render path.
- `Audio` transfers the optional reader exactly once into `Resource`; custom
  readers remain legacy-only.
- the playlist and `LoadTrack` command carry an explicit `TrackBinding`, and a
  bound `PlayerTrack` requests from its exact render subrange while the audible
  output remains on the compatibility ring.

### Transitional bound

The Slice 4 sidecar proves range identity, bounded forward read-ahead, cache
reuse, and safe retirement. While the legacy processed-audio ring remains the
authoritative output, it also bounds decoder progress. Slice 5 must introduce an
explicit session source-authoritative mode before arbitrary reverse prefetch or
prepared elastic output can drive decode independently.

## Slice 5 - Elastic Renderer And Latency Contract

### Goal

Render one bound track from the session-to-track source path and expose
deterministic algorithmic latency.

### Tests

- local beat-map slope produces the expected source advance;
- the selected pitch-preserving backend obeys its declared rate envelope;
- priming makes the first audible beat land at the presentation boundary;
- small phase errors converge without a discontinuous source jump.

## Slice 6 - Multi-Track Tempo Transaction

### Goal

Render two, then four tracks with different source tempos and apply a session
tempo change to all of them on one exact render boundary.

### Tests

- all tracks share one old and one new context revision;
- beat position is continuous at the change;
- tracks remain within the declared phase tolerance after latency compensation;
- no legacy pitch-bend or producer queue acts as a second tempo path.
- tracks with different renderer latency align on one `PresentationFrame`;
- an unsupported tempo or insufficient look-ahead rejects the whole change
  before the Kithara anchor, graph schedule, or any binding mutates.

## Slice 7 - Transactional Session Seek And Explicit Join

### Goal

Prepare source windows and renderer state for every affected track, then commit
a session seek atomically. Add a new track at an explicitly requested
`SessionBeat` join boundary.

### Tests

- no partial old/new position is audible;
- failure to prepare one track leaves the Kithara transport and every binding
  unchanged;
- stale prepared work cannot commit;
- offline preparation completes deterministically;
- an explicit join installs one bound track at the exact requested output frame
  without moving or revising session transport.

## Slice 8 - File-Source Reverse

### Goal

Support per-track reverse through bounded ascending cache ranges and reverse
audible consumption while session time moves forward.

### Tests

- deterministic reverse marker order;
- range requests remain ascending and bounded;
- direction change prepares renderer history before commit;
- unsupported sources fail typed without stale replay.

## Slice 9 - HLS Reverse And Adaptive Read-Ahead

### Goal

Extend range planning across HLS segments, codec access points, preroll,
discontinuities, and
variant changes without leaking transport into protocol code.

### Tests

- reverse across segment boundaries;
- defined source-start behavior and elastic-backend capability rejection;
- bounded cache pressure;
- discontinuity and variant-change policy;
- explicit capability error when the source cannot satisfy reverse demand.

## Slice 10 - Queue Successor Prefetch

### Goal

Derive successor demand from its binding, expected join beat, direction, and
renderer look-ahead.

### Tests

- successor is ready at the join boundary;
- replacing or removing it invalidates stale demand;
- tempo changes update future demand without duplicating cached source audio.

## Slice 11 - Offline Artifact, Real-Time, Performance, And Device Gates

### Goal

Run the complete subsystem through the integration session, persist WAV output,
measure real-time behavior, and validate Apple/device, Android, and browser
WASM surfaces.

### Acceptance

- deterministic multi-track WAV with tempo change, seek, and reverse markers;
- the WAV disk sink is mandatory in this integration lane and remains outside
  the public product API;
- no allocation, blocking, or lock regression in the render callback;
- bounded cache and worker queues under direction and tempo stress;
- repository acceptance gates pass;
- Apple/device and Android output match the shared transport/binding contract;
- browser `wasm32-unknown-unknown`/WebAudio matches the same transport contract.

## Slice 12 - Event Vocabulary Migration

### Goal

Replace the legacy entertainment-role event type with `TransportEvent` and
`SyncEvent` ownership before the new subsystem publishes events. Preserve wire
compatibility only through an explicit FFI migration contract; do not add new
variants to the legacy type.

### Tests

- session tempo/play/seek lifecycle is emitted only by `TransportEvent`;
- binding availability, lock, relock, and direction lifecycle is emitted only
  by `SyncEvent`;
- app and FFI conversion coverage contains no new role-based identifiers.

## Risks And Follow-Ups

- Exact sample-boundary sub-block scheduling through unchanged Firewheel must be
  proven for realtime and offline, including late, stale, abort, and stream-reset
  behavior. Failure cannot be solved by patching Firewheel.
- Elastic algorithm selection and latency may require an explicit backend
  capability matrix before Slice 5.
- HLS reverse can be limited by encryption, codec access-point placement,
  required preroll, or unavailable historical segments; capability reporting
  must remain explicit.
- The plan files are locally ignored by repository policy. Durable contracts
  are copied into owning `CONTEXT.md` files as slices land.
