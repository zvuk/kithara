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

## Orientation

- **Lifecycle:** `start()` → `allocate_slot()` → attach `Player` →
  `replace_current_item(Some(item))` → `play()`; tear down with `release_slot(id)`
  then `stop()`.
- **Crossfade:** `Engine::crossfade(from, to, config)` fades one slot out and
  another in via `CrossfadeConfig` / `CrossfadeCurve`.
- **Tempo & key-lock:** driven by the shared `StretchControls` handle in
  `PlayerConfig.timestretch`; speed, key-lock, and backend apply live, mid-track.
- **Events:** `tokio::sync::broadcast` via `player.subscribe()` /
  `engine.subscribe()` (`PlayerEvent`, `ItemEvent`, `EngineEvent`,
  `SessionEvent`, `DjEvent`).
- **Queue auto-advance:** `PlayerImpl` exposes an `arm_next` / `commit_next`
  handover API for external orchestrators (e.g. `kithara-queue::Queue`).
- **Cancel:** the player's `CancelScope` is derived from `PlayerConfig.cancel`;
  the master cancel lives at the consumer-crate top.

File and HLS pipelines are unconditional; cpal output is the default backend.
Enable `mock` for trait-level `unimock` testing.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
