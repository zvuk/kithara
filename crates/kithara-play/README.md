<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-play.svg)](https://crates.io/crates/kithara-play)
[![docs.rs](https://docs.rs/kithara-play/badge.svg)](https://docs.rs/kithara-play)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-play

The playback orchestration crate behind Kithara. It provides concrete player,
engine, resource, session, and real-time rendering surfaces for queue, FFI, app,
and test-harness crates. Enable `mock` for the `Equalizer` unimock helper.

## Usage

```rust
use kithara_play::{ResourceConfig, default_resource_decoder_config};

let decoder = default_resource_decoder_config();
let resource = ResourceConfig::for_src("https://example.com/track.m3u8")?
    .decoder(decoder)
    .build();
```

`ResourceConfig` fields are crate-private. Configure resources with its `bon`
builder and inspect caller-facing values through getters such as `source()`,
`store()`, and `bus()`. Decoder backend, gapless, and resampler settings belong
to the single `decoder` field.

## Core Surface

- `EngineImpl` owns session dispatch, slot registration, master output state,
  and the shared decode worker.
- `PlayerImpl` owns playlist and parameter state, transport flow, status, and
  item handover.
- `Tempo` and `SessionTransportSnapshot` expose the render-driven musical
  transport shared by every engine in one session.
- `TrackBinding` anchors one active track's analysed beat map to that session
  transport with an immutable direction.
- `Resource` opens file, HLS, and reader sources from `ResourceConfig`.
- `PlayerResource` owns exactly one linear or prepared-bound playback resource;
  bound preparation transfers the original `Resource` without reopening it.
- `PlayerNode` is the public standalone real-time audio graph node. Nodes owned
  by a `SessionState` additionally consume its shared render context.
- `Equalizer` is the remaining mockable trait surface.

## Orientation

- **Lifecycle:** start the engine, allocate a slot, attach a player item, play,
  then release the slot and stop the engine.
- **Configuration:** `PlayerConfig`, `EngineConfig`, and `ResourceConfig` expose
  builders while their fields remain crate-private.
- **Tempo and key-lock:** a shared `StretchControls` handle is supplied through
  `PlayerConfig::builder().timestretch(...)`; speed, key-lock, and backend apply
  live, mid-track.
- **Events:** `tokio::sync::broadcast` via `player.subscribe()` /
  `engine.subscribe()` (`PlayerEvent`, `ItemEvent`, `EngineEvent`,
  `SessionEvent`, `TransportEvent`, `SyncEvent`, `DjEvent`).
- **Queue auto-advance:** `PlayerImpl` publishes `PrefetchRequested` /
  `HandoverRequested`; `kithara-queue::Queue` disables the built-in linear policy
  and selects the loaded successor itself.
- **Cancel:** the player's `CancelScope` is derived from `PlayerConfig.cancel`;
  the master cancel lives at the consumer-crate top.

File and HLS pipelines are unconditional; cpal output is the default backend.
Enable `mock` for `EqualizerMock`.

The role-first source tree is organized as `api/`, `bridge/`, `engine/`,
`player/{state,flow,node,track}/`, `resource/`, `session/{render,web}/`, plus the
target-gated `wasm` surface. Audio-callback code stays with its player or session
owner instead of forming a separate real-time subsystem.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
