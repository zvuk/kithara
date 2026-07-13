# kithara-play — Context

Detailed contracts and invariants for the kithara-play crate; the README is the overview.

## Crossfade

`Engine::crossfade(from, to, config)` fades one slot out while the other fades
in. `CrossfadeConfig` selects the curve (`CrossfadeCurve`: `EqualPower`,
`Linear`, `SCurve`, `ConstantPower`, `FastFadeIn`, `FastFadeOut`), duration,
cut points, and a `beat_aligned` flag. Beat/BPM data types (`BeatGrid`,
`BpmInfo`) and `DjEvent` exist for callers that supply their own analysis.

## Tempo & Key-Lock

`kithara-audio`'s `StretchControls` (one per deck, in `PlayerConfig.timestretch`)
is a single `Arc` holding `speed` — plus `keylock` + `backend` when a stretch
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
  `set_pitch(speed)` — changing speed shifts pitch (vinyl-style).
- **key-lock on**: the stretch slot applies `set_ratio(1/speed)` and
  `set_pitch(1.0)` — changing speed preserves pitch.

At speed 1.0 with no region plan the slot bypasses byte-identically. Without a
stretch backend, including wasm, no speed DSP is inserted and PCM output remains
pinned to 1.0 speed. Mobile feature sets default key-lock to on, so app-level
rate changes are pitch-preserving unless the consumer explicitly disables
key-lock.

Because the controls are read each chunk, **key-lock, backend, and speed all apply
live, mid-track — no reload.** Switching backend rebuilds the DSP backend;
returning to unity passthrough resets buffered stretch state.
`Queue` delegates `set_rate` to the player; key-lock and backend are set by the
consumer directly on the shared `StretchControls` handle.

## Events

`tokio::sync::broadcast`, via `player.subscribe()` / `engine.subscribe()`.

<table>
<tr><th>Enum</th><th>Scope</th></tr>
<tr><td><code>PlayerEvent</code></td><td>Status, rate, volume, mute, current item, prefetch/handover, EOF, failure</td></tr>
<tr><td><code>ItemEvent</code></td><td>Item status, buffering, seek, stall, end-of-stream</td></tr>
<tr><td><code>EngineEvent</code></td><td>Slot lifecycle, crossfade progress, master volume</td></tr>
<tr><td><code>SessionEvent</code></td><td>Interruption, route change, media services</td></tr>
<tr><td><code>DjEvent</code></td><td>BPM detected, beat tick, sync engage/disengage, phase aligned</td></tr>
</table>

Status types: `PlayerStatus`, `TimeControlStatus`, `ItemStatus`,
`WaitingReason`. Audio-session value types (`PortDescription`, `PortType`,
`RouteDescription`, `SessionDuckingMode`) describe routing. Identity/time:
`SlotId`, `ObserverId`, `MediaTime` (a `CMTime` mirror with `Ord` and arithmetic).

## Queue Auto-Advance

`PlayerImpl` exposes a handover API for external orchestrators and internal
tests:

- `arm_next(idx) -> Option<Arc<str>>` — load the next item into the audio thread,
  ready for gapless stitch (cf=0) or parallel fade (cf>0).
- `commit_next(idx) -> Result<(), PlayError>` — promote the armed slot (cf>0 only;
  the audio thread handles cf=0 internally).
- `unarm_next()` — drop the armed slot without committing.
- `armed_next() -> Option<usize>` — snapshot of the armed index.

Two near-end triggers are published: `PlayerEvent::PrefetchRequested` and
`PlayerEvent::HandoverRequested` (emitted only when `crossfade_duration > 0`).
`PlayerConfig::auto_advance_enabled` (default `true`) uses a built-in linear
policy (`next = current + 1`). `kithara-queue::Queue` disables that built-in
policy and reacts to `HandoverRequested` by selecting the loaded successor via
`select_item_with_crossfade`; it does not call `arm_next` / `commit_next`.

## Engine Lifecycle

`start()` → `allocate_slot()` → attach `Player` → `replace_current_item(Some(item))`
→ `play()`. For crossfade, allocate a second slot and call
`Engine::crossfade(from, to, config)`. Tear down with `release_slot(id)` then
`stop()`.

`Engine::start` is **atomic single-start**: the `running` check-then-act is
serialized by an internal `start_lock`, and `running` flips to `true` only after
`session.start_player` has fully succeeded. Two concurrent starters (e.g. the
synchronous `Queue::select` path and an async loader-completion both calling
`PlayerImpl::ensure_engine_started`) cannot both dispatch `start_player`: the
loser observes `running == true` under the lock and returns
`EngineAlreadyRunning`. `ensure_engine_started` treats `EngineAlreadyRunning` as
success — the engine is started, which is all it promises — so a concurrent start
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
that cancels the player's own subtree — it never implicitly cancels a
potentially-foreign master passed in from above (the previous `Drop`-cancel of
the passed token is gone). Hard-coded `CancelToken::root()` and
`CancelToken::never()` outside the allowlist are forbidden, enforced by
`cargo xtask lint arch` (`cancel_root_sites`).

## Real-Time Audio Thread

Two distinct real-time surfaces carry the `#[kithara::rtsan_forbid_blocking]`
contract; both are verified by RealtimeSanitizer (gated by `--cfg rtsan`, so
stable/production builds are byte-identical).

**Consumer `process()` — permanent.** `PlayerNodeProcessor::process` and
`MasterEqProcessor::process` run on the Firewheel audio thread and stay
allocation-, free-, and lock-free: scratch buffers are pre-sized at stream
start, and evicted tracks are handed to a bounded deferred-drop channel (drained
on the main thread) instead of being freed on the audio thread.

**Worker produce-core — verified-after-refactor.** The audio worker's
`produce_pass` (`kithara-audio` scheduler) is `#[rtsan_forbid_blocking]`: the
decode core reads/seeks without malloc/lock/syscall. Off-core work is pushed to
the unchecked **shell** of the run-loop — pooled-buffer free, the deferred
reader-hook + peer-wake flush (`flush_deferred`, run by `recycle`), the parked
`wait`, and the intrinsic symphonia `next_packet` allocation are all
**blocking-by-design in the shell**, never on the forbid path. The reader wakes
the downloader by *arming* a lock-free flag on the core (`Stream::probe_read` /
`probe_seek`); the shell delivers the cross-thread `notify_one`. FSM lifecycle
telemetry (`AudioEvent` via `emit_seek_lifecycle`) is enqueued lock-free on the
core into a `DeferredBus<AudioEvent>` and published by the shell
(`flush_deferred` + `Drop`), so the `EventBus` tokio-broadcast send stays off
the forbid path — like the reader-hooks. The produce-core read/seek path is
verified kevent/yield-free; the CI lane stays advisory (soak) until that holds
across the full lane set.

**Lanes.** `just rtsan` (mock decoder, fast tripwire), `just rtsan-file`
(real-decoder file-offline), `just rtsan-hls` (real-decoder HLS-offline). The
nightly `.github/workflows/rtsan.yml.disabled` runs all three on linux+macos
(pinned nightly + `rust-src`), `continue-on-error` until the produce-core lanes
are green.

**No `kithara-rtsan` crate.** The `permit()` / forbid-blocking macros stay in
`kithara-test-utils` alongside the USDT probe system — it is a normal dependency
of the production crates (most purely for probes), so splitting only the RT
attributes would fragment the shared `kithara::` facade and shed nothing.

## Session Hosting

Platform-asymmetric by necessity. Native (`impls/session/host_native.rs`): a
dedicated engine worker thread drains an `mpsc::Receiver<CmdMsg>`. Ring buffers
live in session state / engine slots, not in the command host. Web
(`impls/session/host_web.rs`): `AudioContext` lives on the browser main thread,
and Worker-side clients proxy commands over an `mpsc` bridge. The cross-platform
core (`state.rs`, `client.rs`) carries zero `#[cfg]`; the structural gates are
the four cfg lines around `mod host_native`, `mod host_web`, and their re-exports
in `mod.rs`.

## Session mixing

`apply_mix(inputs)` is a free function over a set of players that share one audio
session; there is no mixer object. Desired session-input gain stays owned by each
`EngineImpl::master_volume`; applied graph gain stays owned by
`SessionState::PlayerState`. The two are distinct and only cross through one
command.

`Cmd::SetPlayerMasterVolumes` is the single session gain mechanism. `run_cmd`
validates the whole `levels` vector — every value finite and in `0.0..=1.0`,
every player present, no player repeated — before mutating any stored volume or
memo, so a rejected batch changes nothing. The singular `Engine::set_master_volume`
funnels through the same batch with one entry.

`apply_mix` handles the lazily-allocated `PlayerId`: an engine that has never
started has no id, so it is left out of the dispatched subset and only its
desired level is stored. The protocol is: take the session from the first input;
validate levels, same-session (`Arc::ptr_eq`), and uniqueness; take every input's
`start_lock` in a stable
address order (so a batch cannot interleave with a concurrent `start`, and two
overlapping batches cannot deadlock); dispatch one batch for the registered
subset; only on success store every desired level and emit the master-volume
event; release the locks. A subsequent `start` reads the stored desired level. On
dispatch failure nothing is committed.

The session graph places one peak limiter (`impls/limiter_node.rs`, wrapping
`kithara_audio::PeakLimiter`) between the session-output ducking node and the
graph output. There is exactly one per active session context; tearing the
context down (last player stopped, route recreate) drops it, and rebuilding the
session output recreates it with a fresh envelope. Player start/stop only
connects or disconnects player master nodes and never touches the limiter.

## Session mixing

Session-input gain has two distinct owners. Each `EngineImpl` owns its *desired*
input level (`master_volume`, read by `start`). The session `SessionState` owns
the *applied* graph gain (each `PlayerState.master_volume` and its
`VolumePanNode` memo). The only transition between them is one batch command,
`Cmd::SetPlayerMasterVolumes`, which validates the whole vector — every level
finite and in `0.0..=1.0`, every player present, no player repeated — before
mutating any stored volume or memo. Omitted players are unchanged; an invalid
entry leaves the whole batch untouched. This is the single session gain path:
the singular `EngineImpl::set_master_volume` funnels one entry through it.

Session-input levels are linear **amplitudes**: a level of `0.5` halves the
player's amplitude, and the sub-ceiling session output is the exact weighted sum
`Σ levelᵢ · signalᵢ`. Firewheel's `Volume::Linear` is a fader taper (it squares
its argument), so `master_gain` converts a level to that taper before it reaches
the player's `VolumePanNode`. Feeding a level in directly would land `0.5` at
`0.25` amplitude and, worse, turn the equal-power crossfader's `cos`/`sin`
coefficients into `cos²`/`sin²` — no longer equal-power. Slot/content volume and
session ducking keep their own existing taper; they are separate controls.

`apply_mix` is a free function: it actuates the final levels of a set of players
in one batch. The session is taken from the first input; it validates levels,
same-session membership (`Arc::ptr_eq`), and per-batch uniqueness, then takes
every input engine's `start_lock` in a stable address order — so a batch cannot interleave
with a concurrent `start`, and two batches sharing players cannot deadlock. Only
already-registered engines (those with a session `PlayerId`) enter the dispatched
subset; a never-started engine has its desired level stored so its first `start`
adopts it, without speculative registration. Desired levels are committed and the
master-volume event emitted only after the dispatch succeeds.

The crossfader is pure policy (`crossfader_gain`), not state: consumers fold
`trim * mute * crossfader_gain * group_master` into each member level before
calling `apply`. `group_master` is folded per member, never stored as
process-wide state, so one logical group cannot change another group's master.

The session graph carries exactly one peak limiter (`LimiterNode`, wrapping
`kithara_audio::PeakLimiter`) at the final sum, after session ducking and before
the graph output. It is created once per session context in
`create_session_output`; recreating the context (route change, idle teardown)
rebuilds it with a fresh envelope. Player start/stop only connects or
disconnects player master nodes and never creates a per-player limiter.

The limiter is built from fixed valid session constants (ceiling `0.98`, release
`50 ms`) and a `NonZeroU32` stream rate, so its construction cannot fail;
`LimiterProcessor` still holds an `Option<PeakLimiter>` because the audio-thread
`process` may not construct or fail. On the unreachable `None` the node passes
audio through unlimited rather than substituting a different configuration — a
justified degraded mode (audible passthrough beats a hard failure on the
real-time path), not a fallback that hides a state-resolution bug.

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
- At most `Engine::max_slots()` slots allocated at once; at most one active crossfade.
- `Player::slot_id()` is `None` until registered with the engine.
- `MediaTime::INVALID` has `timescale == 0`; arithmetic on invalid times stays invalid.
- Audio-thread `process()` is allocation-, free-, and lock-free.

## Current item

`PlayerEvent::CurrentItemChanged` means the *identity* of the current item
changed — not merely that playback (re)started. A bare `play()` that resumes
the already-current item must **not** emit it: consumers (the queue, which
re-publishes `QueueEvent::CurrentTrackChanged`, and FFI observers) treat the
event as a track switch and do real work on it — e.g. the DJ studio re-analyses
the waveform.

`play()` enforces this with `last_announced_index` (sentinel `usize::MAX` until
the first announce): it emits only when the loaded index differs from the last
announced one, so first activation announces but a resume does not. The genuine
track moves (`commit_next`, `advance_to_next_item`, the handover finaliser, the
jump path) go through `announce_current_item`, which records the index and
emits. Item-set mutations that change identity under a reused index
(`remove_all_items`, `remove_at`, `replace_item*` on the announced index) reset
the sentinel to `usize::MAX`, so the next `play()` re-announces.

## Testing

The offline render backend for deterministic engine/player tests lives in
`kithara-integration-tests::offline`, not here. Enable `mock` for trait-level
`unimock` testing.

## Integration

Defines the public player API consumed by higher-level crates. All traits are
`Send + Sync + 'static`; failures propagate via `Result<T, PlayError>` (no
`unwrap()`/`expect()` in production code).
