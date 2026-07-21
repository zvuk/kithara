# kithara-play - Context

Detailed contracts and invariants for the kithara-play crate; the README is the overview.

## Tempo & Key-Lock

`kithara-audio`'s `StretchControls` (one per deck, in `PlayerConfig.timestretch`)
is a single `Arc` holding `speed` - plus `keylock` + `backend` when a stretch
backend is compiled in (`stretch-signalsmith` / `stretch-bungee`). It is the one
source of truth, shared between the UI and the worker's effect chain, which reads
it each chunk. Rate setters (`set_rate`, `play`) and `set_keylock` / `set_backend`
all write this one handle; there is no second rate atomic and no manual mirror.

`prepare_config` always passes the shared controls into every track (`stretch =
Some(..)`). With a compiled-in backend the effect chain runs a source-domain
`TimeStretchProcessor`; fixed-ratio sample-rate conversion is handled by Apple
codec-embedded decode when that placement is selected, otherwise by the
standalone playback resampler stage:

- **key-lock off**: the stretch slot applies `set_ratio(1/speed)` and
  `set_pitch(speed)` - changing speed shifts pitch (vinyl-style).
- **key-lock on**: the stretch slot applies `set_ratio(1/speed)` and
  `set_pitch(1.0)` - changing speed preserves pitch.

At speed 1.0 with no region plan the slot bypasses byte-identically. Without a
stretch backend, including wasm, no speed DSP is inserted and PCM output remains
pinned to 1.0 speed. Mobile feature sets default key-lock to on, so app-level
rate changes are pitch-preserving unless the consumer explicitly disables
key-lock.

Because the controls are read each chunk, **key-lock, backend, and speed all apply
live, mid-track - no reload.** Switching backend rebuilds the DSP backend;
returning to unity passthrough resets buffered stretch state.
`Queue` delegates `set_rate` to the player; key-lock and backend are set by the
consumer directly on the shared `StretchControls` handle.

## Events

`tokio::sync::broadcast`, via `player.subscribe()` / `engine.subscribe()`.

<table>
<tr><th>Enum</th><th>Scope</th></tr>
<tr><td><code>PlayerEvent</code></td><td>Status, rate, volume, mute, current item, prefetch/handover, EOF, failure</td></tr>
<tr><td><code>ItemEvent</code></td><td>Item status, buffering, seek, stall, end-of-stream</td></tr>
<tr><td><code>EngineEvent</code></td><td>Slot lifecycle, master volume</td></tr>
<tr><td><code>SessionEvent</code></td><td>Interruption, route change, media services</td></tr>
<tr><td><code>TransportEvent</code></td><td>Committed session tempo, seek, play state, and failure facts</td></tr>
<tr><td><code>SyncEvent</code></td><td>Committed track binding facts</td></tr>
<tr><td><code>DjEvent</code></td><td>Legacy BPM analysis, beat tick, key-lock, and stretch-backend facts only</td></tr>
</table>

New transport or synchronization facts must not be added to `DjEvent`.
`TransportEvent` and `SyncEvent` are the canonical vocabulary for the shared
session subsystem. A new event variant requires a committed producer path in
the owning transport or track module; the public event surface does not reserve
future lifecycle states.

Status types: `PlayerStatus`, `TimeControlStatus`, `ItemStatus`,
`WaitingReason`. `SessionDuckingMode` describes ducking policy. Identity:
`SlotId`.

## Queue Auto-Advance

`PlayerImpl` exposes a handover API for external orchestrators and internal
tests:

- `arm_next(idx) -> Option<Arc<str>>` - load the next item into the audio thread,
  ready for gapless stitch (cf=0) or parallel fade (cf>0).
- `commit_next(idx) -> Result<(), PlayError>` - promote the armed slot (cf>0 only;
  the audio thread handles cf=0 internally).
- `unarm_next()` - drop the armed slot without committing.
- `armed_next() -> Option<usize>` - snapshot of the armed index.

Two near-end triggers are published: `PlayerEvent::PrefetchRequested` and
`PlayerEvent::HandoverRequested` (emitted only when `crossfade_duration > 0`).
`PlayerConfig::auto_advance_enabled` (default `true`) uses a built-in linear
policy (`next = current + 1`). `kithara-queue::Queue` disables that built-in
policy and reacts to `HandoverRequested` by selecting the loaded successor via
`select_item_with_crossfade`; it does not call `arm_next` / `commit_next`.

## Planes & Ownership

`api/` owns stable public shapes; `bridge/` owns cross-plane protocol and shared
RT handles; `resource/` owns source, config, and reader construction; `player/`
owns one deck: main-thread playlist and parameter state in `state/`, transitions
in `flow/`, callback-side node/processor/render state under `player/node/`, and
per-track state under `player/track/`. `player/multi/` owns heterogeneous player
composition, routed member handles, topology revision, and group transactions.
`engine/` owns session and slot registration; `session/` owns the physical
player roster, protocol, transport ledger, graph dispatch, shared render
context, master EQ, and platform clients. Real-time execution is a property of
the callback processors, not a separate ownership plane. `wasm` is the
target-gated browser binding surface.

`player`, `engine`, and `session` are intentional orchestration planes: their
entry files import several sibling modules to bind API state, RT controls, and
session commands. The per-crate `module_fan_out` threshold is raised for
`kithara-play`; do not add re-export hops solely to lower that count.

### Player Composition

`Player` is the common object-safe control contract implemented by
`PlayerImpl`, `kithara-queue::Queue`, routed `Member<T>` handles, and
`MultiPlayer`. `PlayerComponent` contributes canonical `PlayerImpl` leaves to a
composition without exposing them to callers. `PlayerComponentBox` is only the
type-erasure boundary used at registration; consumer crates add `From<T>` for
their own component types rather than teaching `kithara-play` those concrete
implementations.

`MultiPlayer::register` consumes the component and returns `Member<T>`. The
caller cannot retain a raw mutable control path to a registered value. Nested
groups attach to one parent and every member resolves its current root before a
routed command, so regrouping cannot create a second command owner. The
composition owns only membership, its monotonic `TopologyRevision`, and one
active tempo-or-seek guard. It does not mirror deck state, queue state, session
tempo, bindings, PCM, or decoder state.

Registration fails closed when a component contributes no canonical
`PlayerImpl` leaf, contributes a duplicate leaf, spans more than one session,
or exposes more than one nested composition root. Consequently every accepted
component participates in the existing player/session ownership path; the
component trait cannot introduce a second transport or player implementation.
The validated leaf roster is frozen in the registration record; later group
transactions never call component code to rediscover or replace that roster
without a `TopologyRevision` change.

`SessionSeek` is the separate musical-relocation capability. Both `MultiPlayer`
and `Member<T>` implement it; a member request is routed through the current
root because a bound deck cannot relocate independently of peers sharing the
session transport. Ordinary unbound `Player::seek_seconds` remains local. A
group-local seek first reads every member duration and rejects a target at or
beyond the shortest member before mutating any component.

`StartAt::Immediate` is the base operation used by `Player::play`.
`StartAt::SessionBeat` schedules a bound player against the shared render
context. A routed `Member<T>::join_track_at`, where `T: Borrow<PlayerImpl>`,
requires an empty deck, prepares the supplied resource through the existing
player/worker/elastic path, and starts it at the requested future beat without
changing session transport. The standard `Borrow` bound covers the canonical
`PlayerImpl` and its `Arc` test/probe handle without adding a marker trait. No
parallel player, queue, source, worker, or allocation layer is created for
composition or join.

### Accepted lint residue

Architecture lint residue is ratcheted and does not license new violations.
The open `SlotControl` and `PlaybackShared` fields preserve the intentional
white-box render API used by the offline harness in
`kithara-integration-tests`. `PlayerImpl` and `PlayerTrack` keep their existing
per-responsibility modules (`ItemQueue`, `Handover`, `Notifier`, `ConfigPrep`,
and `PlayerPhase`); composition and group-session transactions stay under
`player/multi/` rather than widening either owner.

## Engine Lifecycle

`start()` -> `allocate_slot()` -> attach `PlayerImpl` -> `replace_current_item(Some(item))`
-> `play()`. Tear down with `release_slot(id)` then
`stop()`.

`EngineImpl::start` is **atomic single-start**: the `running` check-then-act is
serialized by an internal `start_lock`, and `running` flips to `true` only after
`session.start_player` has fully succeeded. Two concurrent starters (e.g. the
synchronous `Queue::select` path and an async loader-completion both calling
`PlayerImpl::ensure_engine_started`) cannot both dispatch `start_player`: the
loser observes `running == true` under the lock and returns
`EngineAlreadyRunning`. `ensure_engine_started` treats `EngineAlreadyRunning` as
success - the engine is started, which is all it promises - so a concurrent start
is idempotent, never a `"player already started"` session desync.

## Cancel Hierarchy

Cancel is a typed propagate-down tree (`kithara-platform` `common/cancel/`):
`CancelToken` is a `Clone`-by-identity handle, `CancelToken::root()` mints a
fresh tree root, and `cancel()` on any node flags it and cascades down to every
descendant. `is_cancelled()` is a single Acquire-load of the node's own flag.

`PlayerImpl` takes its cancel through `CancelScope::new(config.cancel)`:
a passed `CancelToken` (consumer crates `Queue` / `App` / FFI mint their own
`CancelToken::root()` and pass `root.child()` through `PlayerConfig.cancel`)
makes the scope a composed child of that token; `None` makes the player's scope
token itself a fresh `root()`. Subsystems (Downloader, AssetStore, HlsPeer,
audio worker, epoch cancel) derive children via `.child()` from the scope's
token, so a master / parent `cancel()` is observed by all of them.

`CancelScope::Drop` is **passive**. Teardown is an explicit `scope.cancel()`
that cancels the player's own subtree - it never implicitly cancels a
potentially-foreign master passed in from above (the previous `Drop`-cancel of
the passed token is gone). Hard-coded `CancelToken::root()` and
`CancelToken::never()` outside the allowlist are forbidden, enforced by
`cargo xtask lint arch` (`cancel_root_sites`).

## Real-Time Audio Thread

Two distinct real-time surfaces carry the `#[kithara::rtsan_forbid_blocking]`
contract; both are verified by RealtimeSanitizer (gated by `--cfg rtsan`, so
stable/production builds are byte-identical).

**Consumer `process()` - permanent.** The player-node and master-EQ processors
run on the Firewheel audio thread and stay
allocation-, free-, and lock-free: scratch buffers are pre-sized at stream
start, and evicted tracks are handed to a bounded deferred-drop channel (drained
on the main thread) instead of being freed on the audio thread.

If that deferred-drop channel is full, the processor retains exactly one track
in `pending_retirement` and pauses command draining, cleanup, and further
eviction until the track can be handed off unchanged. Rendering continues.
Explicit stop and clear remove the natural-EOF retention marker first, so
backpressure cannot turn a cleared track into a warm end-of-queue survivor.

**Worker produce-core - verified-after-refactor.** The audio worker's
`produce_pass` (`kithara-audio` scheduler) is `#[rtsan_forbid_blocking]`: the
decode core reads/seeks without malloc/lock/syscall. Off-core work is pushed to
the unchecked **shell** of the run-loop - pooled-buffer free, the deferred
reader-hook + peer-wake flush (`flush_deferred`, run by `recycle`), the parked
`wait`, and the intrinsic symphonia `next_packet` allocation are all
**blocking-by-design in the shell**, never on the forbid path. The reader wakes
the downloader by *arming* a lock-free flag on the core (`Stream::probe_read` /
`probe_seek`); the shell delivers the cross-thread `notify_one`. FSM lifecycle
telemetry (`AudioEvent` via `emit_seek_lifecycle`) is enqueued lock-free on the
core into a `DeferredBus<AudioEvent>` and published by the shell
(`flush_deferred` + `Drop`), so the `EventBus` tokio-broadcast send stays off
the forbid path - like the reader-hooks. The produce-core read/seek path is
verified kevent/yield-free; the CI lane stays advisory (soak) until that holds
across the full lane set.

**Lanes.** `just rtsan` (mock decoder, fast tripwire), `just rtsan-file`
(real-decoder file-offline), `just rtsan-hls` (real-decoder HLS-offline). The
nightly `.github/workflows/rtsan.yml.disabled` runs all three on linux+macos
(pinned nightly + `rust-src`), `continue-on-error` until the produce-core lanes
are green.

**No `kithara-rtsan` crate.** The `permit()` / forbid-blocking macros stay in
`kithara-test-utils` alongside the USDT probe system - it is a normal dependency
of the production crates (most purely for probes), so splitting only the RT
attributes would fragment the shared `kithara::` facade and shed nothing.

## Session Hosting

Platform-asymmetric by necessity. Native (`session/native.rs`): a dedicated
engine worker thread drains an `mpsc::Receiver<CmdMsg>`. Ring buffers live in
session state / engine slots, not in the command host. Web
(`session/web/{bridge,client}.rs`): `AudioContext` lives on the browser main
thread, and Worker-side clients proxy commands over an `mpsc` bridge. The
cross-platform core (`session/{state,dispatch,protocol}.rs`) carries zero
`#[cfg]`; the structural gates are the cfg lines around `mod native`, `mod web`,
and their re-exports in `mod.rs`.

## Session Musical Transport

`SessionState` is the sole control-side transport ledger and revision allocator.
`TransportCommitState` is the sole real-time owner of the analytical anchor
`(render_frame, session_beat, tempo, playing, revision)`. Every `EngineImpl`
connected to one session reaches the ledger through the typed session protocol;
player and queue state do not mirror it. Firewheel owns only the execution
graph, render clock, sample-rate observation, and event delivery.

`Tempo` rejects non-finite and non-positive values before a commit is created.
The first tempo command targets the current graph frame. Later changes target
the current graph frame plus one declared maximum callback, preserving the beat
at that exact frame and changing only the analytical slope from that frame
onward. Firewheel musical transport, dynamic keyframes, and transport speed are
not part of this contract. `MultiPlayer` owns the public group mutation surface:
checked tempo changes and the `SessionSeek` relocation capability. Routed
player join is a prepared deck operation and does not mutate transport. Group
pause/resume transactions remain outside this contract; `Player::pause`,
`play`, and `start_at` compose the existing member controls.

Successful tempo commits advance one monotonic revision; repeating the same
tempo is a no-op. The control side sends an immediate revisioned `Stage` event
and a scheduled `Apply` event for the target frame. The pre-process adapter
accepts `Apply` only when its revision, previous commit, sample rate, and graph
frame match the staged transaction exactly. Late, stale, aborted, duplicate, or
route-invalidated transactions cannot change the active anchor. Rejection keeps
the previously observed commit authoritative and is returned as a typed session
error.

Multi-player tempo changes extend the existing player ownership chain rather
than introducing a composition registry in the session. `MultiPlayer` collects
the canonical leaves through `PlayerComponent`, locks their existing
`ItemQueue` values in the frozen registration order, and validates every
current bound track, including paused tracks. The checked session participant
IDs are sorted independently before protocol dispatch. Tempo and session-seek transactions derive their
participants from the same active physical-roster predicate: registered decks
without a started engine and slot do not belong to the graph roster and do not
enter the checked player-ID set. The preflight uses the same
`bound_plan::plan_elastic_segments` path as rendering for the old span through
the future boundary and the first new-tempo span after it, so marker-local rate
limits and the decoded source frontier are checked before any transport
mutation.

The guarded session command rechecks the observed revision, stream shape, and
the active graph-owned player IDs before it queues the ordinary transport
stamp. A future transport commit blocks new bound-track preparation until the
commit is observed. A local player handover with two audible tracks rejects a
tempo change explicitly because one control-side current binding cannot
describe both callback tracks. These are transient checks over player- and
session-owned state; the session stores no copy of player bindings or readiness.

Session seek uses one immutable private plan containing the target, observed
snapshot, preparation context, and candidate `TransportRevision`. Every
participant allocates a monotonic `SessionSeekAttempt` from its persistent
`PlaybackShared` lane. The callback starts and polls the canonical bounded
source request using preallocated `PcmPool` buffers; the existing Audio worker
performs the decode/seek work. The control path waits for every attempt and
revalidates the same slot, item, binding, roster, stream shape, and transport
context before the session queues one boundary commit. Attempt IDs are
preparation handshakes, not transport revisions. Cancellation is a direct
atomic handshake with the callback owner, so a saturated command ring cannot
leave a stale prepared relocation. Any failure cancels all matching attempts
and leaves the previously observed transport authoritative. A successful apply
publishes `TransportEvent::SeekCommitted` with the exact target and revision.

Each prepared elastic reader consumes its declared source history and
discards its declared output latency before becoming audible. The resulting
presentation cursor is therefore latency-compensated per track, while every
track continues to receive the same immutable session context and revision.
The browser path retains the same player API but rejects a current bound track
with `ElasticBackendUnavailable` until a browser elastic backend exists. Its
prepared-bound state is uninhabited, so browser code cannot construct a fake
ready or active reader.

`SessionTransportSnapshot` is published as one render-observed value by the
same adapter node. It never combines a control-side tempo or revision with an
independently sampled graph clock. Until a candidate is applied, a query returns
the last complete snapshot; before the first processed commit it returns
`TransportNotProcessed`. Stream stop rejects any pending boundary, preserves
the last processed beat, and reanchors the active commit on the first frame of
the replacement route. Stopping the final engine ends the session transport
lifetime and resets the control ledger and real-time state together.

`kithara-audio::TrackBeatMap` owns the immutable analysed relationship between
track beats and host-rate source frames. Each active `PlayerTrack` may own one
`TrackBinding`, which composes that map with a session anchor, track anchor,
and playback direction. In forward playback the track phase is
`track_anchor + (session - session_anchor)`; reverse playback changes only that
sign. The session owns no per-track map, and the binding is coordinate state
rather than another tempo or clock. Missing or invalid analysis is a typed
`SyncUnavailable` result, never a BPM-derived fallback.

`TrackAxis` makes the active binding and its host sample rate one construction
value, so a `PlayerTrack` cannot start with mismatched coordinate axes. A bound
track cannot mutate that axis after a host sample-rate change: it retains the
prepared rate and fails closed against the new session context until it is
re-prepared. Unbound tracks update through the ordinary route-change path.

The playlist carries an optional immutable `TrackBinding` beside its resource.
For a bound item, `PlayerImpl` transfers the original `Resource` into off-callback
elastic preparation. `kithara-audio::elastic::ElasticReader<Preparing>` polls
bounded ranges through the resource's existing `PcmReader` implementation and
can become `ElasticReader<Ready>` only through the ready outcome. Activation
consumes that ready state and stores `ElasticReader<Active>` inside the
player-owned `ActiveBoundReader`; release restores `ElasticReader<Ready>` to the
queued item. The same resource and reader move together through every
transition. No
reopen recipe, second decoder, or isolated event scope exists. Custom readers
either implement the canonical bounded-read capability or fail with its typed
unsupported error.

`PlayerImpl::insert_with_binding` owns this asynchronous preparation. Success
means the queued item already has a fully primed reader stamped with one
atomic `(transport revision, sample rate, maximum block size)` snapshot.
Preparation failure leaves the playlist unchanged. Slot existence and command
capacity are checked before activation; either rejection restores the dormant
reader, while activation failure restores the same complete queue entry. The
playlist lock spans take through command publication, so insert, remove, and
replacement linearize entirely before or after the load; rollback cannot target
a shifted or replaced coordinate.

`ItemQueue` also owns successor demand. A future bound entry keeps one binding,
one prepared reader, and its original resource; the binding session anchor is
its intended start coordinate. Initial preparation retains the maximum backend warm-up
history together with the normal directional look-ahead. The exact-tempo range
and only the additional envelope history are fetched as separate bounded demands
into the same prepared PCM window. Before a tempo commit, the player validates
every future bound reader against the candidate tempo and current stream
shape. When a later selection observes the committed revision, it re-primes
that same dormant reader from the retained PCM window and updates its stamp
without reopening the resource or duplicating cached source audio. Replacement
and removal discard the whole queue entry, so no stale successor demand survives
either mutation.

`player::track::bound_plan` maps `TrackBinding`, `SessionBeat`, and
`RenderContext` into transport-neutral `ElasticAnchor` and `ElasticSpan`
values. `ActiveBoundReader` decorates the audio reader only with player-owned
request identity, `TransportRevision`, and the pending session relocation
target/revision. It does not own PCM windows, a DSP backend, or another source
cursor.

`kithara-audio::elastic::ElasticReader` is the sole owner of the audible
directional source cursor, bounded source windows, and exact-span backend. It
reads through the canonical `Resource`/`Audio` seek epoch using the existing
`PcmReader` traits. The backend declares its rate envelope and deterministic
latency and is primed before activation. Decoder position and legacy pitch bend
are not parallel phase authorities for a bound item.

The bound `PlayerResource` owns one `Resource` and one prepared reader, while
the linear variant owns one `Resource` and its pooled planar scratch. These
storage modes cannot coexist. Bound reads use the shared `AudioWorker`, PCM ring,
seek epoch, trash ring, and existing `PcmPool` owned by `kithara-audio`;
`kithara-play` adds no source thread, cache, port protocol, allocator, or player
infrastructure. The audio reader rotates a fixed preallocated `PcmBuf` bank.
Polling and deadline failure never allocate, block, or return a buffer to the
pool on the audio callback.

Bound elastic rendering supports native forward playback and reverse playback
for files and single-variant HLS. Activation requires a session-created node
and its shared `RenderContext`; browser WASM returns a typed backend capability
error. Every source request remains one bounded ascending `SourceRange`. A
reverse reader primes history and the configured number of successor windows
before publication, consumes each window toward lower frames, and replenishes
the same preallocated bounded queue with earlier ascending ranges. A ready
successor never replaces the active PCM window before the render cursor crosses
their overlap. Reaching source frame zero produces the defined end boundary and
never wraps or replays stale PCM.

HLS remains the owner of byte-to-segment resolution, codec access points,
preroll, cache pressure, and discontinuity fences; the player submits only
decoded source-frame ranges through the same `Resource` lane used by file
reverse. Reverse HLS is admitted only when the resolved HLS peer exposes one
variant. Adaptive/multi-variant HLS is rejected with
`ReverseSourceUnavailable` before playlist publication until reverse ABR
re-aiming across variant discontinuities has its own proven protocol contract.
Other resources that do not declare reverse range access fail through the same
typed capability gate. Ordinary track-local seek is rejected for a bound
current item because it would bypass the shared transport transaction;
`SessionSeek` prepares and commits every affected reader together.

Inter-block phase correction is bounded in the source-frame domain by
`ElasticSpanConfig`, supplied through `PlayerConfig`'s existing
`ElasticReaderConfig`. Its defaults accept and correct at most one source frame
per maximum render block, scaled for sub-blocks and constrained by the backend
rate envelope. The integer span always begins at the previous cursor, so
correction cannot jump over source. A larger error requires prepared
relocation; absence of rate-envelope headroom is a typed failure rather than
silent drift. The numeric quantization algorithm belongs to `kithara-stretch`,
and the audio reader commits its staged cursor only after all planned segments
render. The player owns the session target/revision transaction and tells the
reader to publish a prepared relocation only at the matching transport
boundary.

Ordinary bound preparation uses the transport commit already observed by the
render graph. Before the session has any active commit, it may use the first
accepted commit because that commit targets the initial render boundary. Once an
active commit exists, a later accepted-but-pending tempo never replaces it in a
preparation query. A multi-track transaction must stage every participant before
publishing a later revision.

One zero-I/O Kithara graph-adapter node, executed as a Firewheel pre-process
node, creates the immutable `RenderContext` for each processed graph block.
`ProcStore` is only its preallocated delivery
slot: the pre-process node is the sole writer, and every session-created player
node reads the same value before passing it unchanged to each `PlayerTrack`.
The context owns the signed render-frame range, host sample rate, optional
session-beat range, and the matching immutable session transport commit. An
absent or paused transport is valid and produces no beat range. Subranges retain
the same commit. The public standalone `PlayerNode` and
`PlayerNodeProcessor::new` keep their context-free render contract; only
`SessionState` opts nodes into the required session context.

The slot distinguishes unwritten, valid, and invalid state. Every callback
replaces the prior state, so invalid input cannot reuse a stale snapshot.
Player processing also requires an exact render-frame and sample-rate match;
node-local scheduled-event sub-blocks are fail-closed until their context
derivation has an explicit contract. Handover subranges derive matching signed
frame and session-beat intervals from the callback snapshot; using the full
callback beat range for a partial render is a phase error. Musical context stops
at the track render boundary and does not enter resource, stream, HLS, or decode
reads.

## Route Changes

`PlayerNodeProcessor::new_stream` is the host-rate bridge. A platform route
notification updates the session host sample rate and propagates it to every
loaded resource. When the numeric rate changes, resources enter the existing
decoder recreate path with `RecreateCause::RouteChange`; the same machinery
preserves playback position and gapless state. Equal-rate notifications only
refresh the host state and do not recreate. The recreated decoder receives the
current host rate through `DecoderConfig.resampler`: Apple fused builds use the
codec-embedded placement, while non-fused builds use the standalone decoder
adapter placement with the selected backend.

`ResourceConfig.decoder` is the only resource-level owner of decoder
construction settings. Its `AudioDecoderConfig.resampler` threads the selected
backend handle and implementation tunables into `AudioConfig`, then into
`DecoderConfig.resampler`. The portable default backend order is owned by
`kithara-resampler`; callers that want a platform backend such as Apple
AudioConverter inject it through `ResourceConfig.decoder.resampler`, not
through separate resource-level resampler fields.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>backend-cpal</code></td><td>yes</td><td>CPAL output via <code>firewheel/cpal</code></td></tr>
<tr><td><code>backend-web-audio</code></td><td>no</td><td>WebAudio backend (wasm32)</td></tr>
<tr><td><code>wasm-bindgen</code></td><td>no</td><td>WASM bindings via <code>firewheel/wasm-bindgen</code></td></tr>
<tr><td><code>symphonia</code></td><td>yes</td><td>Software decode forwarding to <code>kithara-audio</code> and <code>kithara-decode</code></td></tr>
<tr><td><code>fdk-aac</code></td><td>no</td><td>FDK-AAC decode forwarding to <code>kithara-audio</code> and <code>kithara-decode</code></td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox decode via <code>kithara-audio/apple</code> and <code>kithara-decode/apple</code>; does not imply Rubato</td></tr>
<tr><td><code>apple-fused-src</code></td><td>no</td><td>Apple AudioToolbox fused decode+SRC through decoder-embedded resampler placement</td></tr>
<tr><td><code>resample-rubato</code></td><td>yes</td><td>Default fixed-ratio Rubato backend for playback decode adapters and beat analysis in default builds</td></tr>
<tr><td><code>resample-glide</code></td><td>no</td><td>Glide resampler backend forwarding for explicit config selection without Rubato</td></tr>
<tr><td><code>analysis-beat</code></td><td>yes</td><td>Beat-analysis pass forwarding to <code>kithara-audio</code>; absent from Apple FFI device sets</td></tr>
<tr><td><code>analysis-waveform</code></td><td>yes</td><td>RealFFT waveform analyzer forwarding to <code>kithara-audio</code></td></tr>
<tr><td><code>client-reqwest</code></td><td>yes</td><td>Forward the reqwest HTTP backend to network-reaching deps</td></tr>
<tr><td><code>client-wreq</code></td><td>no</td><td>Forward the wreq HTTP backend to network-reaching deps</td></tr>
<tr><td><code>tls-rustls</code></td><td>yes</td><td>Forward rustls TLS selection to network-reaching deps</td></tr>
<tr><td><code>tls-native</code></td><td>no</td><td>Forward native TLS selection to network-reaching deps</td></tr>
<tr><td><code>probe</code></td><td>no</td><td>USDT runtime tracing</td></tr>
<tr><td><code>mock</code></td><td>no</td><td><code>unimock</code> trait mocks</td></tr>
</table>

File and HLS pipelines are unconditional: `kithara-play` always links
`kithara-file`, `kithara-hls`, `kithara-abr`, `kithara-assets`, `kithara-net`.

## Invariants

- `SlotId` is valid only between `allocate_slot()` and `release_slot()`.
- At most `EngineImpl::max_slots()` slots allocated at once.
- `PlayerImpl::slot()` is `None` until registered with the engine.
- Audio-thread `process()` is allocation-, free-, and lock-free.

## Current item

`PlayerEvent::CurrentItemChanged` means the *identity* of the current item
changed - not merely that playback (re)started. A bare `play()` that resumes
the already-current item must **not** emit it: consumers (the queue, which
re-publishes `QueueEvent::CurrentTrackChanged`, and FFI observers) treat the
event as a track switch and do real work on it - e.g. the DJ studio re-analyses
the waveform.

`Playlist` owns both the current index and the announce-dedup state. Its
`last_announced: Option<usize>` starts as `None`; `mark_announced` records an
index and reports whether it changed. `announce_current_item` is the sole event
publisher and emits only when that report is true, so first activation
announces but a resume does not. Genuine track moves (`commit_next`,
`advance_to_next_item`, the handover finaliser, and the jump path) route through
that publisher. Playlist mutations that can change identity under a reused
index (`clear`, `remove_at`, and replacement of the announced index) reset the
dedup state to `None`, so the next `play()` re-announces.

## Testing

The offline render backend for deterministic engine/player tests lives in
`kithara-integration-tests::offline`, not here. Enable `mock` for trait-level
`unimock` testing. Bound-render acceptance uses that existing integration
session to write a WAV artifact; it is a validation surface, not a product
offline-rendering feature.

## Integration

Defines the public player API consumed by higher-level crates. Failures
propagate via `Result<T, PlayError>` (no `unwrap()`/`expect()` in production
code).
