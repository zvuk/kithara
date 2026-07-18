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
<tr><td><code>DjEvent</code></td><td>BPM detected, beat tick, sync engage/disengage, phase aligned</td></tr>
</table>

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
owns main-thread playlist and parameter state in `state/` and transitions in
`flow/`, while its callback-side node, processor, and render pass live under
`player/node/` and per-track state under `player/track/`; `engine/` owns session and slot registration;
and `session/` owns protocol, state, graph dispatch, shared render context,
master EQ, and platform clients. Real-time execution is a property of the
callback processors, not a separate ownership plane. `wasm` is the target-gated
browser binding surface.

`player`, `engine`, and `session` are intentional orchestration planes: their
entry files import several sibling modules to bind API state, RT controls, and
session commands. The per-crate `module_fan_out` threshold is raised for
`kithara-play`; do not add re-export hops solely to lower that count.

### Accepted lint residue

The live architecture lint warnings retained after the role-first split are
intentional: four `single_impl_size` warnings are layout-inherent after the
fine-grained module split; two `mixed_entities` warnings reflect modules whose
paired entities share one owner and lifecycle; and the two
`pub_struct_open_fields` warnings on `SlotControl` and `PlaybackShared` preserve
the intentional white-box render API used by the offline harness in
`kithara-integration-tests`. `PlayerImpl` and `PlayerTrack` were decomposed
into per-responsibility owners (`ItemQueue`, `Handover`, `Notifier`,
`ConfigPrep`, and the `PlayerPhase` typestate), so no `god_struct` warning
remains.

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
not part of this contract. The public session mutation surface changes tempo
and performs transactional session seek; pause/resume transactions are outside
this slice.

Successful tempo commits advance one monotonic revision; repeating the same
tempo is a no-op. The control side sends an immediate revisioned `Stage` event
and a scheduled `Apply` event for the target frame. The pre-process adapter
accepts `Apply` only when its revision, previous commit, sample rate, and graph
frame match the staged transaction exactly. Late, stale, aborted, duplicate, or
route-invalidated transactions cannot change the active anchor. Rejection keeps
the previously observed commit authoritative and is returned as a typed session
error.

Multi-player tempo changes extend the existing player ownership chain rather
than introducing a participant registry. `PlayerImpl::set_session_tempo`
receives the participating players, locks their existing `ItemQueue` values in
`PlayerId` order, and validates every current bound track, including paused
tracks. The preflight uses the renderer's `plan_elastic_segments` path for the
old span through the future boundary and the first new-tempo span after it, so
marker-local rate limits and the decoded source frontier are checked before any
transport mutation.

The guarded session command rechecks the observed revision, stream shape, and
the active graph-owned player IDs before it queues the ordinary transport
stamp. A future transport commit blocks new bound-track preparation until the
commit is observed. A local player handover with two audible tracks rejects a
tempo change explicitly because one control-side current binding cannot
describe both callback tracks. These are transient checks over player- and
session-owned state; the session stores no copy of player bindings or readiness.

`SessionTrackControl::seek_session` is implemented by `PlayerImpl`; it owns no
state and introduces no alternate command or rendering path. The implementation
acquires the existing participant `ItemQueue` locks in `PlayerId` order,
captures each current binding and slot, and publishes `PrepareSessionSeek`
through the existing slot command lanes while queue identities remain stable.
It releases the playlist locks before waiting for callback-side preparation,
then reacquires them and verifies the exact slot, current index, binding,
transport revision, and stream shape before the checked session commit.

Preparation uses the existing track's `ElasticRenderer`. A second source port
fills a dormant relocation window while the first port continues serving the
audible cursor; there is no second renderer or phase authority. The transport
commit carries the requested seek target as well as its revision, so stale seek
work cannot match an unrelated tempo commit that allocates the same next
revision. Any failure after preparation starts sends `CancelSessionSeek`
through each original slot lane, leaves the session transport unchanged, and
allows a later retry.

Each prepared elastic renderer consumes its declared source history and
discards its declared output latency before becoming audible. The resulting
presentation cursor is therefore latency-compensated per track, while every
track continues to receive the same immutable session context and revision.
The browser path retains the same player API but rejects a current bound track
with `ElasticBackendUnavailable` until a browser elastic backend exists.

`SessionTrackControl::join_track_at` adds one bound resource to an idle
`PlayerImpl`; joining over an existing playlist or an elapsed beat is rejected.
The native player prepares the resource and its existing `ElasticRenderer` at
the requested `SessionBeat`, then holds the `ItemQueue` playlist lock from
insertion through publication on the existing slot command lane. A rejected
publication removes that exact inserted entry, so no partially joined queue
state remains.

The callback stores only the pending join beat and observed transport revision
on the new `PlayerTrack`. The immutable `RenderContext` maps that beat to the
first matching output frame; the track stays silent before that offset and
begins rendering at the offset within the same callback. Join never commits or
reanchors session transport. A changed revision rejects preparation before
publication, while a stale revision observed by the callback fails the joined
track through the existing playback failure path. Native join ownership lives
in `player/platform/native.rs`; the separate browser implementation in
`player/platform/wasm.rs` returns `ElasticBackendUnavailable` without adding
native-only queue state to the WASM path.

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
For a bound stream-backed item, `ResourceBlueprint` opens an independent reader
and prepares its source-audio lane and elastic renderer off the audio callback.
The prepared renderer is transferred to the active `PlayerTrack`. A blueprint is
a passive recipe: every opened `Resource` owns a separate cancellation child,
and dropping the recipe cannot cancel an opened reader. Internal preparation
also opens with an isolated event bus, so its decoder seeks and lifecycle events
cannot enter the application-facing resource event scope. Custom readers have
no reconstruction contract: unbound custom readers retain the compatibility
path, while bound insertion returns `BindingSourceNotReopenable`.

`PlayerImpl::insert_with_binding` owns this asynchronous preparation. Success
means the queued item already has a fully primed renderer stamped with one
atomic `(transport revision, sample rate, maximum block size)` snapshot.
Preparation failure leaves the playlist unchanged. Slot existence and command
capacity are checked before activation; either rejection restores the dormant
renderer, while activation failure restores the same complete queue entry. The
playlist lock spans take through command publication, so insert, remove, and
replacement linearize entirely before or after the load; rollback cannot target
a shifted or replaced coordinate.

`ElasticRenderer` is the sole owner of the audible directional source cursor.
For each render range it derives the desired source path from `TrackBinding` and
the shared `RenderContext`, then submits an integer source/output span to a
pitch-preserving backend. The backend declares its rate envelope and
deterministic latency, is primed before activation, and reads immutable bounded
source windows from a dedicated non-real-time worker. Decoder position, legacy
pitch bend, and the processed-audio compatibility ring are not parallel phase
authorities for a bound item.

The source worker is a dedicated named platform thread started only after the
slot command lane accepts activation. Its lifetime is independent of Tokio and
is owned by the source port's cancellation and activity signal. It snapshots
`SourceAudioActivity` before probing cancellation, requests, source readiness,
or reply capacity and parks only while the predicate remains unresolved.
Successful request, recycle, service, and reply-consume edges signal the same
activity from the real-time side through nonblocking `ThreadGate::signal`;
decoded data, terminal status, and producer close are signaled by
`kithara-audio`. No periodic timer or runtime `Notify` participates in this
path.

Bound elastic rendering supports native forward and file-source reverse
playback. Activation requires a session-created node and its shared
`RenderContext`; browser WASM returns a typed backend capability error. Every
source-worker request remains one bounded ascending `SourceFrameRange`. A
reverse renderer primes history in reverse audible order, consumes each window
toward lower frames, and renews with an earlier ascending range. Reaching source
frame zero produces the defined end boundary and never wraps. Resources that do
not declare reverse range access fail with `ReverseSourceUnavailable` before
playlist publication; HLS remains unsupported until its protocol-specific
range contract lands. Ordinary track-local seek is rejected for a bound current
item because only a session relocation transaction may move the session-owned
audible cursor and renderer state.

Inter-block phase correction is bounded in the source-frame domain. An error of
at most one source frame is corrected continuously at no more than one frame per
maximum render block, scaled for sub-blocks and constrained by the backend rate
envelope. The integer span always begins at the previous cursor, so correction
cannot jump over source. A larger error requires prepared relocation; absence of
rate-envelope headroom is a typed failure rather than silent drift.

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
