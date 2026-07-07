# kithara-play — Context

Detailed contracts and invariants for the kithara-play crate; the README is the overview.

## Tempo & Key-Lock

`kithara-audio`'s `StretchControls` (one per deck, in `PlayerConfig.timestretch`)
is a single `Arc` holding `speed` — plus `keylock` + `backend` when a stretch
backend is compiled in (`stretch-signalsmith` / `stretch-bungee`). It is the one
source of truth, shared between the UI and the worker's effect chain, which reads
it each chunk. Rate setters (`set_rate`, `play`) and `set_keylock` / `set_backend`
all write this one handle; there is no second rate atomic and no manual mirror.

`prepare_config` always passes the shared controls into every track (`stretch =
Some(..)`). With a compiled-in backend the effect chain runs a
`TimeStretchProcessor` in tempo mode whether or not key-lock is on:

- **key-lock off** (default): the stretch slot bypasses (PCM forwarded untouched)
  and routes `speed` to the resampler — changing speed shifts pitch (vinyl-style).
- **key-lock on**: the stretch slot changes tempo with pitch held and pins the
  resampler to 1.0.

Without one (default native build, wasm) there is no key-lock: the resampler
follows the shared speed atomic directly, identical to key-lock off.

Because the controls are read each chunk, **key-lock, backend, and speed all apply
live, mid-track — no reload.** Switching key-lock or backend resets the FFT
backend's buffer, so a brief transient (~100–300 ms) is expected at the switch.
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

## Planes & Ownership

`api/` owns stable public shapes, `bridge/` owns cross-plane protocol and shared
RT handles, `resource/` owns source/config/reader construction, `player/` owns
main-thread queue and flow state, `engine/` owns session/slot registration, and
`rt/` owns lock-free audio-node rendering.

`player`, `engine`, and `session` are intentional orchestration planes: their
entry files import several sibling modules to bind API state, RT controls, and
session commands. The per-crate `module_fan_out` threshold is raised for
`kithara-play`; do not add re-export hops solely to lower that count.

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

Platform-asymmetric by necessity. Native (`session/native.rs`): a dedicated
engine worker thread drains an `mpsc::Receiver<CmdMsg>`. Ring buffers live in
session state / engine slots, not in the command host. Web
(`session/web/{bridge,client}.rs`): `AudioContext` lives on the browser main
thread, and Worker-side clients proxy commands over an `mpsc` bridge. The
cross-platform core (`session/{state,dispatch,protocol}.rs`) carries zero
`#[cfg]`; the structural gates are the cfg lines around `mod native`, `mod web`,
and their re-exports in `mod.rs`.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>backend-cpal</code></td><td>yes</td><td>CPAL output via <code>firewheel/cpal</code></td></tr>
<tr><td><code>backend-web-audio</code></td><td>no</td><td>WebAudio backend (wasm32)</td></tr>
<tr><td><code>wasm-bindgen</code></td><td>no</td><td>WASM bindings via <code>firewheel/wasm-bindgen</code></td></tr>
<tr><td><code>symphonia</code></td><td>yes</td><td>Software decode forwarding to <code>kithara-audio</code> and <code>kithara-decode</code></td></tr>
<tr><td><code>fdk-aac</code></td><td>no</td><td>FDK-AAC decode forwarding to <code>kithara-audio</code> and <code>kithara-decode</code></td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox decode via <code>kithara-audio/apple</code></td></tr>
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

Defines the public player API consumed by higher-level crates. Failures
propagate via `Result<T, PlayError>` (no `unwrap()`/`expect()` in production
code).
