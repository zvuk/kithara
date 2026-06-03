<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-play.svg)](https://crates.io/crates/kithara-play)
[![docs.rs](https://docs.rs/kithara-play/badge.svg)](https://docs.rs/kithara-play)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-play

The player engine behind kithara. A trait-first API modelled on Apple's
AVPlayer, with multi-slot mixing, sample-accurate crossfade, and pro-audio
features. Ships default implementations (`EngineImpl`,
`PlayerImpl`, `Resource`); the core traits are mockable via `unimock` (the
`mock` feature).

## Usage

```rust
use kithara_play::{Engine, Player, CrossfadeConfig};

engine.start()?;
let slot_a = engine.allocate_slot()?;

player.replace_current_item(Some(item));
player.play();

let id = player.add_periodic_time_observer(interval, Box::new(|time| {
    println!("position: {time:?}");
}));

// Crossfade into a second slot
let slot_b = engine.allocate_slot()?;
engine.crossfade(slot_a, slot_b, CrossfadeConfig::default())?;

engine.release_slot(slot_a)?;
engine.stop()?;
```

## Core Traits

An `Engine` manages multiple playback slots; each slot drives one `Player` over
a `PlayerItem`. The engine owns master output and crossfade; a per-slot
`Equalizer` shapes the sound.

<table>
<tr><th>Trait</th><th>AVPlayer analogue</th><th>Role</th></tr>
<tr><td><code>Player</code></td><td><code>AVPlayer</code></td><td><code>play</code>, <code>pause</code>, <code>seek</code>, <code>rate</code>, <code>volume</code>, <code>replace_current_item</code>, time observers</td></tr>
<tr><td><code>PlayerItem</code></td><td><code>AVPlayerItem</code></td><td>status, current time, duration, loaded ranges, seek, buffering</td></tr>
<tr><td><code>QueuePlayer</code></td><td><code>AVQueuePlayer</code></td><td><code>items</code>, <code>advance_to_next_item</code>, <code>insert</code>, <code>remove</code></td></tr>
<tr><td><code>Engine</code></td><td>—</td><td>Lifecycle, slot arena, master output, <code>crossfade</code></td></tr>
<tr><td><code>Equalizer</code></td><td>—</td><td>N-band parametric EQ with per-band gain</td></tr>
</table>

`Player` and `QueuePlayer` take `Box<dyn Fn>` time-observer callbacks, so they
are integration-tested rather than `unimock`ed.

## Crossfade

`Engine::crossfade(from, to, config)` fades one slot out while the other fades
in. `CrossfadeConfig` selects the curve (`CrossfadeCurve`: `EqualPower`,
`Linear`, `SCurve`, `ConstantPower`, `FastFadeIn`, `FastFadeOut`), duration,
cut points, and a `beat_aligned` flag. Beat/BPM data types (`BeatGrid`,
`BpmInfo`) and `DjEvent` exist for callers that supply their own analysis.

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
`WaitingReason`. Audio-session value types (`SessionCategory`, `SessionMode`,
`SessionOptions`, `PortType`, `RouteDescription`) describe routing. Identity/time:
`SlotId`, `ObserverId`, `MediaTime` (a `CMTime` mirror with `Ord` and arithmetic).

## Queue Auto-Advance

`PlayerImpl` exposes a handover API for external orchestrators (e.g.
`kithara-queue::Queue`):

- `arm_next(idx) -> Option<Arc<str>>` — load the next item into the audio thread,
  ready for gapless stitch (cf=0) or parallel fade (cf>0).
- `commit_next(idx) -> Result<(), PlayError>` — promote the armed slot (cf>0 only;
  the audio thread handles cf=0 internally).
- `unarm_next()` — drop the armed slot without committing.
- `armed_next() -> Option<usize>` — snapshot of the armed index.

Two near-end triggers are published: `PlayerEvent::PrefetchRequested` (arm the
next track) and `PlayerEvent::TrackHandoverRequested` (commit it; emitted only
when `crossfade_duration > 0`). `PlayerConfig::auto_advance_enabled` (default
`true`) uses a built-in linear policy (`next = current + 1`); orchestrators turn
it off and drive advance themselves.

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

A single `CancellationToken` master flows top-down; subsystems (Downloader,
AssetStore, HlsPeer, audio worker, epoch cancel) derive children via
`.child_token()`, and cancelling the master cascades to all of them. When used
directly, `PlayerImpl::new` is the canonical fallback owner (`unwrap_or_default()`,
marked `// kithara:cancel:owner`); consumer crates (`Queue`, `App`, FFI) build
their own master and pass it through `PlayerConfig.cancel`. `Drop for PlayerImpl`
pulses `cancel()` before teardown. Hard-coded `CancellationToken::new()` outside
marked owner/bridge sites is forbidden.

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
dedicated engine worker thread drains a `ringbuf` of commands. Web
(`impls/session/host_web.rs`): `AudioContext` lives on the browser main thread,
and Worker-side clients proxy commands over an `mpsc` bridge. The cross-platform
core (`state.rs`, `client.rs`) carries zero `#[cfg]`; the only structural gate is
two lines in `mod.rs`.

## Feature Flags

<table>
<tr><th>Feature</th><th>Default</th><th>Effect</th></tr>
<tr><td><code>backend-cpal</code></td><td>yes</td><td>CPAL output via <code>firewheel/cpal</code></td></tr>
<tr><td><code>backend-web-audio</code></td><td>no</td><td>WebAudio backend (wasm32)</td></tr>
<tr><td><code>wasm-bindgen</code></td><td>no</td><td>WASM bindings via <code>firewheel/wasm-bindgen</code></td></tr>
<tr><td><code>apple</code></td><td>no</td><td>Apple AudioToolbox decode via <code>kithara-audio/apple</code></td></tr>
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

## Testing

The offline render backend for deterministic engine/player tests lives in
`kithara-integration-tests::offline`, not here. Enable `mock` for trait-level
`unimock` testing.

## Integration

Defines the public player API consumed by higher-level crates. All traits are
`Send + Sync + 'static`; failures propagate via `Result<T, PlayError>` (no
`unwrap()`/`expect()` in production code).
