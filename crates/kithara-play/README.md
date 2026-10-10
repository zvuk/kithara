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
worker, resource, and session surfaces for queue, FFI, app, and test-harness
crates; the real-time deck it drives lives in `kithara-render`. Enable `mock` for the `Equalizer` unimock helper.
Enable `perf` on native profiling builds for permanent `hotpath` timing at the
playback worker boundary; ordinary builds compile the probes out.

## Usage

### Configure a resource

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

### Read decoded audio

The async resource constructor opens the configured source and exposes its
metadata, audio specification, and decoded samples through the same interface.

```rust
use kithara_assets::AssetStore;
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_play::{PlayWorker, PlayWorkerConfig, Resource, ResourceConfig, ResourceSrc};

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

// Auto-detect: .m3u8 -> HLS, everything else -> progressive file
let config: ResourceConfig<AppPools> = ResourceConfig::for_src(ResourceSrc::parse(
    "https://example.com/song.mp3",
)?)
.store(AssetStore::builder(pools).build())
.worker(worker)
.build();
let mut resource = Resource::new(config).await?;

let spec = resource.spec();
let meta = resource.metadata();

let mut buf = [0.0f32; 1024];
resource.read(&mut buf);
```

## Key Types

- `PlayWorker` owns playback pools and a dedicated dispatcher derived from an
  optional shared `kithara-worker` base.
- `HostedDeck` and `DeckPass` transfer deck custody between the player and
  its Host; `SessionOutputView` exposes the Host output snapshot.
- `PlayerImpl` owns playlist and parameter state, transport flow, status, item
  handover, and one clone of its explicitly supplied `PlayWorker`.
- `Resource` opens file, HLS, and reader sources from `ResourceConfig`.
- `PlayerNode`, `CrossfadeSettings`, and the deck's bridge types are
  re-exported from `kithara-render`, which owns them.
- `policy` owns domain-aware cache identity and DRM request routing above the
  filesystem, network, and cryptography crates.
- `Equalizer` is the remaining mockable trait surface.

## Integration

- **Lifecycle:** a Host seats the deck on the slot it builds as it takes the
  deck; attach a player item and play; the Host drops the slot as it hands the
  deck back.
- **Configuration:** `PlayerConfig`, `PlayWorkerConfig`, and `ResourceConfig` expose
  builders while their fields remain crate-private.
- **Tempo and key-lock:** `PlayerConfig::builder().warp(...)` supplies the
  `WarpConfig` every track's renderer starts from, with its key-lock and
  backend. A speed change goes to each held track's render lane and applies on
  a frame of its output, mid-track. Render quantum and rate smoothing remain
  optional frame-based Warp settings. Player resolves an
  unspecified render quantum to 32 frames when `response_budget_frames` is
  supplied. This optional application constraint retains admission checks
  against the actual Host output shape.
- **Events:** `tokio::sync::broadcast` via `player.subscribe()` /
  the Host subscriptions (`PlayerEvent`, `EngineEvent`,
  `SessionEvent`, `DjEvent`).
- **Successors:** a player never advances on its own. Its owner arms the next
  item (`arm_next`) and commits it (`commit_next`); `kithara-queue::Queue` does
  this ahead of the current item's end.
- **Cancel:** the player's `CancelScope` is derived from `PlayerConfig.cancel`;
  the master cancel lives at the consumer-crate top.

File and HLS pipelines are unconditional; cpal output is the default backend.
Enable `mock` for `EqualizerMock`.

The role-first source tree is organized as `api/`, `player/`, `resource/`, and
`session`, plus the
target-gated `wasm` surface. Concrete output-session state,
graph dispatch, and platform clients live in `kithara-host`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-play) for detailed contracts, invariants, and internals.
