<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-play.svg)](https://crates.io/crates/kithara-play)
[![docs.rs](https://docs.rs/kithara-play/badge.svg)](https://docs.rs/kithara-play)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-play

The player engine behind kithara. It provides the concrete playback, resource,
engine, and equalizer surfaces used by queue, FFI, and app crates. Enable
`mock` for the `Equalizer` unimock helper.

## Usage

```rust
use kithara_events::EventBus;
use kithara_play::{EngineConfig, EngineImpl};

let engine = EngineImpl::new(EngineConfig::default(), EventBus::default());
engine.start()?;
let slot = engine.allocate_slot()?;
engine.release_slot(slot)?;
engine.stop()?;
```

## Core Surface

- `EngineImpl` owns the session client, slots, master output, and shared decode
  worker.
- `PlayerImpl` owns queue state, transport commands, status snapshots, and
  item handover.
- `Resource` opens file/HLS/reader sources for the player.
- `Equalizer` is the remaining mockable trait surface.

## Orientation

- **Lifecycle:** start the engine, allocate a slot, attach a player item, play,
  then release the slot and stop the engine.
- **Tempo & key-lock:** driven by the shared `StretchControls` handle in
  `PlayerConfig.timestretch`; speed, key-lock, and backend apply live, mid-track.
- **Events:** `tokio::sync::broadcast` via `player.subscribe()` /
  `engine.subscribe()` (`PlayerEvent`, `ItemEvent`, `EngineEvent`,
  `SessionEvent`, `DjEvent`).
- **Queue auto-advance:** `PlayerImpl` publishes `PrefetchRequested` /
  `HandoverRequested`; `kithara-queue::Queue` disables the built-in linear policy
  and selects the loaded successor itself.
- **Cancel:** the player's `CancelScope` is derived from `PlayerConfig.cancel`;
  the master cancel lives at the consumer-crate top.

File and HLS pipelines are unconditional; cpal output is the default backend.
Enable `mock` for `EqualizerMock`.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
