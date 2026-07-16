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
`flow/`; `engine/` owns session and slot registration; `rt/` owns lock-free
audio-node rendering, with per-track state under `rt/track/`; and `session/`
owns protocol, state, graph dispatch, and platform clients. `wasm` is the
target-gated browser binding surface.

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

**Consumer `process()` - permanent.** `PlayerNodeProcessor::process` and
`MasterEqProcessor::process` run on the Firewheel audio thread and stay
allocation-, free-, and lock-free: scratch buffers are pre-sized at stream
start, and evicted tracks are handed to a bounded deferred-drop channel (drained
on the main thread) instead of being freed on the audio thread.

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
not part of this contract. The current public session mutation surface changes
tempo only; pause/resume transactions are outside this slice.

Successful tempo commits advance one monotonic revision; repeating the same
tempo is a no-op. The control side sends an immediate revisioned `Stage` event
and a scheduled `Apply` event for the target frame. The pre-process adapter
accepts `Apply` only when its revision, previous commit, sample rate, and graph
frame match the staged transaction exactly. Late, stale, aborted, duplicate, or
route-invalidated transactions cannot change the active anchor. Rejection keeps
the previously observed commit authoritative and is returned as a typed session
error.

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
`unimock` testing.

## Integration

Defines the public player API consumed by higher-level crates. Failures
propagate via `Result<T, PlayError>` (no `unwrap()`/`expect()` in production
code).
