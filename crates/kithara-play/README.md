<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-play.svg)](https://crates.io/crates/kithara-play)
[![docs.rs](https://docs.rs/kithara-play/badge.svg)](https://docs.rs/kithara-play)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-play

The playback orchestration crate behind Kithara. It provides concrete player,
engine, resource, session, and real-time rendering surfaces for queue, FFI, app,
and test-harness crates. Enable `mock` for the `Equalizer` unimock helper.
Enable `perf` on native profiling builds for permanent `hotpath` timing at the
playback worker boundary; ordinary builds compile the probes out.

## Usage

```rust
use kithara_assets::AssetStore;
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_play::{PlayWorker, PlayWorkerConfig, ResourceConfig, ResourceSrc};

pool_schema! {
    pub AppPools {
        bytes: u8,
        samples: f32,
    }
}

let config = || PoolConfig::builder().max_buffers(128).build();
let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(config())
    .samples(config())
    .build()?;
let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
let resource: ResourceConfig<AppPools> = ResourceConfig::for_src(ResourceSrc::parse(
    "https://example.com/track.m3u8",
)?)
    .store(AssetStore::builder(pools).build())
    .worker(worker)
    .build();
```

The composition root registers a closed pool schema once. Every playback
component receives the cloneable `PoolRegion` facade, while byte and sample
allocations continue to compete under one shared hard byte budget.

`ResourceConfig` fields are crate-private. Configure resources with its `bon`
builder and inspect caller-facing values through getters such as `source()`,
`store()`, and `bus()`. Decoder backend, gapless, and resampler settings belong
to the single `decoder` field.

## Key Types

- `PlayWorker` owns playback pools and a dedicated dispatcher derived from an
  optional shared `kithara-worker` base.
- `EngineImpl` owns session dispatch, slot registration, and master output
  state.
- `PlayerImpl` owns playlist and parameter state, transport flow, status, item
  handover, and one clone of its explicitly supplied `PlayWorker`.
- `Resource` opens file, HLS, and reader sources from `ResourceConfig`.
- `PlayerNode` is the public real-time audio graph node.
- `policy` owns domain-aware cache identity and DRM request routing above the
  filesystem, network, and cryptography crates.
- `Equalizer` is the remaining mockable trait surface.

## Integration

- **Lifecycle:** start the engine, allocate a slot, attach a player item, play,
  then release the slot and stop the engine.
- **Configuration:** `PlayerConfig`, `EngineConfig`, and `ResourceConfig` expose
  builders while their fields remain crate-private.
- **Tempo and key-lock:** a `WarpConfig` containing the shared
  `StretchControls` handle and render quantum is supplied through
  `PlayerConfig::builder().warp(...)`; speed, key-lock, and backend apply
  live, mid-track.
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

The role-first source tree is organized as `api/`, `bridge/`, `engine/`,
`effects/`, `player/{state,flow}/`, `resource/`, `rt/{track}/`, `session/`, and
`worker/`, plus the target-gated `wasm` surface. Concrete output-session state,
graph dispatch, and platform clients live in `kithara-host`.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
