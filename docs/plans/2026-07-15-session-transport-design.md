# 2026-07-15-session-transport-design

## Intent

Kithara needs a session-scoped musical transport suitable for a multi-track
performance application. The session has a virtual timeline with a
mathematically exact beat grid. The grid is derived from the current tempo and
an anchor; it is never stored as a precomputed list of beat markers. Changing
tempo re-anchors the same beat at an exact render frame and changes the grid
slope from that frame onward.

Tracks are bound to this session grid through their own analysed beat maps.
Two or more tracks can therefore be rendered against one transport snapshot,
stretched independently, and heard on the same musical phase.

The existing offline integration session is a first-class acceptance surface.
It must exercise the same transport and render path as a device session and may
write rendered audio to disk for deterministic inspection.

This document supersedes
`docs/plans/2026-07-03-transport-beat-sync-design.md`. The older document remains
historical evidence for the consume-side pitch-bend work; its session clock,
coordinator, event vocabulary, forward-only limitation, and grid ownership are
not the contract for the new subsystem.

## Locked Decisions

- `SessionState` is the sole control-side ledger and revision allocator for
  Kithara's `SessionTransport`. `TransportCommitState` is the sole real-time
  owner of the analytical anchor
  `(render_frame, session_beat, tempo, playing, revision)`.
- `PlayerImpl` and `ItemQueue` remain the sole control-side owners of track and
  playlist state. `PlayerNodeProcessor` and `PlayerTrack` remain their real-time
  counterparts. Session transport must not mirror track membership, bindings,
  preparation state, or queue mutation state.
- Multi-track changes extend the existing player command path. They must not
  introduce a second participant registry, transaction ledger, renderer model,
  or public track-control protocol beside the player.
- Kithara owns the musical clock, tempo authority, and commit revisions. It
  derives them from the graph render clock without maintaining a parallel
  mutable host-time or beat clock.
- Firewheel remains an unmodified execution graph behind a narrow adapter. The
  adapter supplies render-frame and sample-rate observations and delivers
  Kithara commits at exact render boundaries; it owns no musical state.
- Firewheel musical transport, dynamic keyframes, and transport speed are not
  part of the subsystem. No Firewheel fork, private patch, or upstream change
  is an implementation dependency.
- Truce is reference material for process-context, real-time snapshot, precise
  event, and headless-test patterns. It is not a Kithara dependency or a graph
  or transport provider.
- The complete subsystem must support Apple platforms, Android, and browser
  `wasm32-unknown-unknown`. The same requirement applies to any future
  transport or graph replacement; a candidate missing Android or browser
  support cannot own the complete subsystem.
- The session grid is analytical. At any render frame it is derived from the
  current anchor, tempo, and sample rate.
- A tempo change is one transaction at one render-frame boundary. All tracks
  observe the same new transport snapshot.
- Track analysis never becomes the session clock. It supplies an immutable
  `TrackBeatMap` used to bind one track to the session.
- Decoding, streaming, and HLS remain transport-independent. They receive
  explicit source-frame range demand rather than musical transport state.
- Source decoding remains forward. Reverse playback consumes bounded decoded
  source-audio windows in reverse order.
- Reverse is initially a per-track direction while the session transport moves
  forward. Reversing the entire session is a separate future feature.
- The existing consume-side pitch bend is retained as legacy player control. It
  is not the session tempo or phase actuator and the new sync path is not built
  on it.
- No new identifiers use the legacy decoded-sample acronym. Existing public API
  names are migrated separately.
- No subsystem identifier uses entertainment-role terminology. Names describe
  transport, binding, rendering, cache, or synchronization responsibilities.

## Vocabulary

- `SessionTransport`: session-wide musical clock, tempo, play state, and current
  beat position.
- `SessionTransportSnapshot`: immutable observation of the transport at a
  processed render position.
- `RenderContext`: immutable context created once for a render sub-block and
  passed unchanged to every participating track.
- `TrackBeatMap`: immutable mapping between analysed track beats and canonical
  source frames.
- `TrackBinding`: per-active-track relationship between a track beat anchor and
  a session beat anchor, including audible direction.
- `TrackAxis`: construction value that keeps a track binding and its host-rate
  source-frame axis consistent.
- `SourceAudioCache`: transport-independent cache of decoded, gapless-normalized,
  host-rate source audio.
- `SourceAudioWindow`: bounded cached source range made available to a renderer.
- `SourceFrameRange`: half-open range in the canonical source-audio coordinate
  space.
- `AudioBlock`: rendered host-rate audio for a render-frame range.
- `DirectionalCursor`: cursor that advances through source windows in the
  binding's audible direction without changing decoder direction.
- `ElasticRenderer`: per-track renderer that follows the requested source phase
  and rate while preserving pitch when configured.
- `TransportEvent`: lifecycle and state-change event emitted by the session
  transport owner.
- `SyncEvent`: track-binding, lock, relock, or unavailability event emitted by
  the synchronization owner.

## Coordinate Spaces

Raw integers must not cross these boundaries without an explicit conversion:

- `RenderFrame`: host output frame currently processed by the session graph.
- `PresentationFrame`: frame on the common audible session axis after declared
  renderer and output latency compensation. Musical commits target this axis.
- `SessionBeat`: continuous beat coordinate on the virtual session timeline.
- `SourceFrame`: canonical frame in decoded, normalized, host-rate source audio.
- `TrackBeat`: continuous coordinate in a track's analysed beat map.

Nominal time is metadata, not a substitute for any of these coordinates.
Wrappers or domain types must make conversions visible at API boundaries.
`RenderFrame` and `PresentationFrame` are related only by the session-owned
latency map. A track may not add or subtract its latency ad hoc.

## Analytical Session Grid

For an anchor `(anchor_frame, anchor_beat)`, sample rate `sample_rate`, and tempo
`tempo`, the beat at render frame `frame` is:

```text
beat(frame) = anchor_beat
            + (frame - anchor_frame) * tempo / (sample_rate * 60)
```

The inverse mapping is derived from the same state. No beat array is generated
or cached.

At a tempo change committed at frame `change_frame`:

1. Evaluate `change_beat` using the old state.
2. Install `(change_frame, change_beat, new_tempo)` as the new anchor.
3. Continue rendering from that exact beat with the new slope.

Kithara's `SessionTransport` performs this continuity-preserving re-anchor.
Kithara validates that tempo is finite and positive before staging the change.
The graph adapter applies and publishes the prepared revision at the exact
render boundary; desired state is committed only after the real-time transport
owner reports that revision applied.

Transport observation is render-driven. If no frames are processed, offline
wall-clock time must not advance the beat position.

## Ownership And Data Flow

```text
SessionState / SessionTransport
        +-- control observation --> SessionTransportSnapshot
        |
        +-- Firewheel adapter / unmodified graph
                    |
                    +-- render frame + sample rate --> RenderContext writer
                                                   --> ProcStore snapshot
                                                   |
                                  +----------------+----------------+
                                  |                                 |
                                  v                                 v
                     TrackBinding + TrackBeatMap       TrackBinding + TrackBeatMap
                                  |                                 |
                                  v                                 v
                         ElasticRenderer A                 ElasticRenderer B
                                  |                                 |
                                  +------------- mixer -------------+
                                                        |
                                                    AudioBlock
```

- `SessionState` owns mutation of tempo, play state, and transport position.
- One adapter-installed pre-process node combines Firewheel's render frame and
  sample rate with the current Kithara transport state and replaces a
  preallocated `ProcStore` slot. It does not use Firewheel transport state as
  musical authority. Player nodes read that same snapshot and never query a
  transport independently per track.
- A valid context contains a signed render-frame range, host sample rate, and
  an optional session-beat range. No active transport is a valid context.
- Handover and event subranges derive their own frame and beat interval from
  the callback snapshot; a partial track render never consumes the full
  callback beat range.
- Missing, unwritten, invalid, or block-mismatched context fails closed instead
  of reusing the previous snapshot. This also guards future node-local
  scheduled-event sub-blocks until their derivation is explicitly supported.
- Each active track owns its `TrackBinding` and renderer state.
- Analysis owns immutable `TrackBeatMap` data, not playback state.
- The audio/cache layer owns `SourceAudioCache` and range availability, not
  musical decisions.
- Stream, HLS, and decode layers continue to own byte access, demuxing,
  decoding, normalization, and source seeking.

## Track Beat Mapping And Binding

`TrackBeatMap` maps analysed markers `(TrackBeat, SourceFrame)` and interpolates
between valid neighbouring markers. It preserves real downbeats and meter
information when available. A tempo override cannot manufacture a beat map.
When a required map is absent or invalid, synchronization returns a typed
`SyncUnavailable` result.

`TrackAnalysis` retains the decoded source sample rate that defines its raw
marker axis. `TrackBeatMap` converts that axis explicitly to the host-rate
`SourceFrame` domain; it never assumes that source and host rates match. Actual
beat markers define phase, the first valid downbeat is `TrackBeat(0)`, and
pickup markers before it receive negative beat coordinates. Meter is reported
only when consecutive downbeat spans agree. Markers beyond the decoded source
extent are rejected, and coordinates outside the first-to-last marker domain
remain unmapped rather than extrapolating a synthetic phase.

`TrackBinding` defines at minimum:

- the session beat used as the binding anchor;
- the corresponding track beat;
- audible direction (`Forward` or `Reverse`).

Bindings have an explicit `Prepared` to `Active` lifecycle. A successor may own
a prepared binding before it is audible. That lifecycle owner allocates an
immutable binding revision; Slice 2 does not fabricate an unused initial
revision. The same prepared value is atomically activated at its explicitly
requested `SessionBeat`; activation does not rebuild or reinterpret its anchors.

For each presentation range, the binding converts session beat demand to track beat
demand, then `TrackBeatMap` converts that to a source-frame path. This path, not
a free-floating rate multiplier, is the phase authority for the renderer.

## Render Context And Transactions

`RenderContext` is the graph-facing immutable projection of Kithara's
`SessionTransport`. It contains the values required by all tracks for one
adapter sub-block, including its render-frame range and transport snapshot. It
is constructed once and shared by value or immutable reference.

Control changes use prepare/commit semantics:

1. Validate the request and allocate a session commit revision owned by
   `SessionState`.
2. Resolve every affected track's binding and source demand.
3. Prepare cache windows and renderer state off the real-time path.
4. Commit one immutable state change at an exact `PresentationFrame` boundary.
5. Make stale reads and prepared states harmless through their owning revision.

The three revision domains are distinct:

- session commit revision: owned by `SessionState`, orders atomic musical state
  commits and the `RenderContext` snapshots created from them;
- binding revision: owned by the active or prepared track, identifies immutable
  anchor and direction data;
- demand generation: owned by `SourceAudioCache`, rejects stale asynchronous
  range completions and is never playback or binding state.

Each `ElasticRenderer` declares a supported rate envelope and required
look-ahead. The planner maintains enough source reserve for changes inside that
envelope. A tempo request outside the envelope, or one without the required
reserve, is rejected as a typed preparation failure before the common commit.
It is never delayed for only a subset of tracks. Seek waits for every required
window before commit and never mixes old and new track positions.

If preparation fails for any affected track, the active Kithara transport
anchor and revision, the graph schedule, and all bindings remain unchanged.
There is no partial commit.

## Source Audio Cache And Read-Ahead

`kithara-audio` owns the `SourceAudio` contract and `SourceAudioCache`.
`SourceAudio` is decoded, gapless-normalized, resampled to the current host rate,
and indexed by `SourceFrame`. Decode and resampling components perform those
transformations, but the canonical boundary and cache identity belong to
`kithara-audio`.

`SourceAudioCache` stores immutable source ranges independently of session
tempo, phase, and direction. Its identity includes the source and canonical
audio specification; transport revisions do not duplicate cached audio.

For the compatibility transition, a stream-backed resource extracts an opt-in
`SourceAudioReader` sidecar from `kithara-audio`. The decoder worker owns a
fixed-buffer `SourceAudioTap`; the resource side owns the cache and range-read
port. Unbound resources retain the existing post-effects reader. Slice 5
transfers one native forward bound item to a source-authoritative elastic path;
custom readers remain on the compatibility path and cannot be reopened for
bound preparation.

### Explicit Source Activity Wake

The accepted wake contract is owner-specific and generation-based.
`kithara-audio` owns `SourceAudioActivity`; decoded data, status changes, and
producer close each advance and signal that activity. Real-time-side port edges
that can make the elastic source worker progress call `ThreadGate::signal`.
A dedicated named platform thread owns the blocking worker independently of
Tokio. It snapshots the relevant activity generation, rechecks cancellation
and available work, and waits only while that generation remains unchanged.
This prevents a signal between the work check and the wait from being lost.

The implementation validates decoded data, terminal status, producer close,
every real-time port edge, cancellation during a pending wait, lost-wake
ordering, and Tokio runtime teardown while a source port remains alive. Tokio
`Notify` on the real-time path was rejected because it crosses into
executor/waker machinery. Periodic timer polling was rejected because it adds
wall-clock latency and idle wakeups and makes faster-than-real-time offline
progress nondeterministic. Neither Tokio notification nor timers are part of
the real-time wake contract.

Demand is expressed as ordered `SourceFrameRange` values. A planner derives the
ranges from:

- the current binding and transport horizon;
- the renderer's source-rate requirement and look-ahead;
- audible direction;
- seek or loop boundaries;
- protocol-neutral access constraints and source capabilities supplied by an
  adapter.

The common planner never reads HLS segment, discontinuity, or variant state.
The HLS adapter translates neutral range demand into protocol policy and reports
capability or access constraints back across that boundary.

Forward playback requests ranges beyond the high edge. Reverse playback
requests earlier ranges while every individual decode request remains a normal
ascending range. A `DirectionalCursor` chooses consumption order.

Seek increments demand generation and cancels or ignores obsolete in-flight work.
Already cached immutable ranges remain reusable. Completion for an old generation
must never move a live cursor or overwrite a newer binding.

Each demand token also records its canonical lane and expected decoder seek
epoch. A new generation issued before the decoder commits a seek cannot accept
audio from the previous landing. Window coordinates come from the normalized
host-rate chunk metadata rather than from a synthetic playback cursor.

Queue successor prefetch is derived from the successor's binding and expected
join point. It is added after seek, direction, and elastic look-ahead semantics
are proven; byte-count-only prefetch is insufficient.

## Elastic Rendering And Multi-Track Tempo Changes

Each track has an independent `ElasticRenderer`. Its requested source path for
the current `RenderContext` determines both rate and phase. For a locally linear
beat-map segment, the requested source advance is derived from source frames per
track beat multiplied by session beats per render frame and binding direction.

All renderers receive a tempo change at the same render boundary. Each renderer
may have different source tempo, marker spacing, algorithmic state, and
latency, but none may choose its own session anchor.

Renderers report deterministic algorithmic latency. The session latency map and
mixer compensate each track against one `PresentationFrame` boundary. A renderer
is primed before a binding becomes audible; a worker queue cannot make one track
adopt new tempo later than another.

Phase correction follows one policy:

- small error: bounded continuous correction inside `ElasticRenderer`;
- large error, seek, or new binding: prepared discontinuous relocation with
  renderer reset and a defined transition at commit;
- no parallel phase actuator through legacy pitch bend, decoder position
  polling, or a second tempo owner.

### Current Slice 5 Boundary

The implemented elastic slice proves one native, stream-backed, forward bound
track. Browser WASM bound insertion returns the typed
`ElasticBackendUnavailable` result. Reverse preparation, multi-track readiness,
latency alignment across tracks, and an all-or-nothing tempo transaction remain
owned by their later slices. The complete subsystem still retains the Android
and browser requirements above; the current native-only backend is an explicit
intermediate boundary, not a platform fallback.

## Seek

Slice 7 owns the transactional seek contract below. Until it lands, ordinary
track-local seek on the current bound item returns
`BoundTrackSeekRequiresSessionTransport` and leaves the player position and
session-owned audible cursor unchanged.

A session seek targets `SessionBeat` and a common `PresentationFrame` commit
boundary. Preparation resolves the target source frame for every bound track,
requests renderer look-ahead on both sides as required, resets
history-dependent renderer state, and commits all tracks with Kithara's
`SessionTransport` at that one adapter-enforced boundary.

Real-time rendering may continue the old committed state while preparation is
pending. It must not expose a partially prepared target. Offline rendering may
drive preparation to completion deterministically before requesting the next
block. The completion event identifies the committed revision and beat.

A per-track relocation changes only that track's binding anchor. It does not
seek the session transport.

If any track cannot prepare, the seek fails as one transaction: Kithara's
`SessionTransport`, its graph schedule, and every track remain on the previous
committed revision.

## Reverse Playback

Slice 8 owns file-source reverse. The current Slice 5 preparation path accepts
only `Forward`; a reverse binding returns the typed
`ReversePreparationRequired` result before playlist publication.

Per-track reverse changes the `TrackBinding` direction while the session beat
continues increasing. The source path therefore moves toward lower
`SourceFrame` values as render frames increase.

Decoders are never asked to emit frames backward. The cache obtains bounded
ascending ranges, and the directional cursor exposes them to the renderer in
reverse audible order. Renderer history is prepared at the direction-change
boundary so the transition is deterministic.

Slice 8 implements and validates file-source reverse before Slice 9 extends it
to HLS. HLS adds segment, codec access-point, and preroll planning, bounded
cache pressure, discontinuity handling, and variant policy. A source that
cannot satisfy reverse demand returns a typed capability error; it does not
silently switch direction or replay stale audio. At source start, reverse
reaches a defined boundary result and does not wrap unless an explicit loop
binding says so. The selected elastic backend must declare reverse support
before a reverse binding can be prepared.

## Events And Failures

Events describe committed facts, not requested intent:

- `TransportEvent`: tempo committed, play state committed, session seek
  committed, or transport failure.
- `SyncEvent`: binding committed, lock acquired/lost, relock committed, or sync
  unavailable.

Invalid tempo, missing beat map, unsupported reverse source, unavailable source
range, stale revision, and renderer preparation failure are typed outcomes.
There is no fallback from analysed mapping to a synthetic tempo-only grid.

## Offline Acceptance

The offline integration session uses the same `SessionState`, Kithara
`SessionTransport`, unchanged Firewheel graph through its adapter, bindings,
cache, renderers, and mixer as a device session.
The acceptance harness must render an `AudioBlock` stream to a WAV file for
marker and phase inspection. This disk sink remains a test surface, not a
product feature.

Required acceptance scenarios are:

- 120 BPM advances exactly one beat in 22,050 rendered frames at 44.1 kHz;
- two engines in one offline session observe the same transport position;
- a 120 to 60 BPM change is beat-continuous and changes slope immediately;
- result is independent of render block partitioning;
- two and then four synthetic tracks remain phase-aligned across a tempo change;
- session seek commits every track on the same render boundary;
- file playback supports deterministic per-track reverse;
- offline output can be written to disk and inspected without a device;
- Apple/device output passes the shared contracts that do not depend on
  offline scheduling;
- Android output passes the same shared transport and render contracts;
- browser WebAudio/WASM passes the same shared transport and render contracts.

## Migration

1. Preserve the rebased consume-side pitch-bend behavior as a legacy,
   independently tested control.
2. Replace the transitional Firewheel musical-transport authority with the
   Kithara-owned `SessionTransport`; retain Firewheel only as an unchanged graph
   behind the adapter.
3. Add `TrackBeatMap` and `TrackBinding` without connecting them to legacy player
   rate control.
4. Introduce the source-audio cache and explicit range-demand boundary while
   keeping forward output unchanged.
5. Introduce per-track elastic rendering, latency compensation, and shared
   render context.
6. Add multi-track tempo change, seek, reverse, HLS read-ahead, and successor
   prefetch in independently testable slices.
7. Retire the legacy role-labelled event enum as affected call sites migrate;
   transport facts use `TransportEvent` and binding/lock facts use `SyncEvent`.

Durable ownership and coordinate contracts move into the relevant crate
`CONTEXT.md` files as their implementation slices land.

## Non-Goals

- Precomputing the session beat grid.
- Passing musical transport into stream, HLS, decode, or low-level sample-read
  traits.
- Reversing the whole session in the first implementation.
- Depending on Firewheel musical transport, dynamic keyframes, or transport
  speed.
- Forking or privately patching Firewheel.
- Adding Truce or replacing the graph in this wave.
- Replacing all existing legacy decoded-sample API names in this feature.
- Adding analysis models or changing beat detection.
- Hiding missing analysis behind tempo overrides or synthetic markers.
