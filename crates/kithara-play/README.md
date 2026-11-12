<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-play.svg)](https://crates.io/crates/kithara-play)
[![docs.rs](https://docs.rs/kithara-play/badge.svg)](https://docs.rs/kithara-play)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-play

The player. A `PlayerImpl` holds a playlist of items, controls transport
(play, pause, seek, rate), and hands decoded tracks to its deck; `PlayWorker`
is the shared worker that decodes, resamples, and time-stretches every loaded
track. A player plays once it is inserted into a `kithara-host` Host.

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

// `.m3u8` opens as HLS, anything else as a progressive file.
let track: ResourceConfig<AppPools> =
    ResourceConfig::for_src(ResourceSrc::parse("https://example.com/track.m3u8")?)
        .store(AssetStore::builder(pools).build())
        .worker(worker)
        .build();
```

For a playlist with crossfade use `kithara-queue`, which drives a player.

## Key Types

- `PlayerImpl` / `PlayerConfig` — the player and its builder.
- `PlayWorker` / `PlayWorkerConfig` — shared decode worker and its buffer pools.
- `ResourceConfig` / `ResourceSrc` — one track: source, cache, network,
  decoder options.
- `Resource` — decoded PCM from one source through the `AudioReader`
  read/seek interface.
- `HostedDeck` / `DeckPass` — how a Host takes and returns a player's deck.
- `PlayerEvent`, `EngineEvent`, `SessionEvent`, `DjEvent` — events on
  `player.subscribe()` and the Host subscriptions.

## Features

- `symphonia` (default), `apple`, `webcodecs`, `fdk-aac` — decoders.
- `resample-rubato` (default), `resample-glide` — resamplers.
- `stretch-signalsmith` (default), `stretch-bungee`, `stretch-glide`,
  `stretch-identity` — time-stretch engines.
- `client-reqwest` (default), `client-wreq`, `tls-rustls` (default),
  `tls-native` — HTTP and TLS.
- `mock` — test helpers in `kithara_play::mock` (also enables the audio, render, and stream mocks).

## Integration

A player never advances to the next item on its own: its owner arms the next
item (`arm_next`) and commits it (`commit_next`). `kithara-queue` does this
ahead of the current item's end. Tempo and key lock come from the `WarpConfig`
given to `PlayerConfig::builder().warp(...)`; a speed change applies on an
exact output frame, mid-track. The output graph and audio device belong to
`kithara-host`; the audio-thread deck to `kithara-render`.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-play) for detailed contracts, invariants, and internals.
