<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-queue.svg)](https://crates.io/crates/kithara-queue)
[![docs.rs](https://docs.rs/kithara-queue/badge.svg)](https://docs.rs/kithara-queue)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-queue

A playlist deck in the style of `AVQueuePlayer`. A `Queue` drives a
`kithara-play` player: it loads tracks ahead in the background, moves through
them in order or shuffled, repeats, and crossfades from one track into the
next.

## Usage

```rust
use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_host::{Host, HostConfig};
use kithara_play::{PlayWorker, PlayWorkerConfig, ResourcePrep};
use kithara_queue::{Queue, QueueConfig, Transition};

pool_schema! {
    pub AppPools {
        bytes: u8,
        samples: f32,
    }
}

let pool = || PoolConfig::builder().max_buffers(128).build();
let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
    .bytes(pool())
    .samples(pool())
    .build()?;
let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());

let mut host = Host::new(HostConfig::builder().build())?;
let queue = host.insert(Queue::new(
    QueueConfig::builder()
        .prep(ResourcePrep::builder().worker(worker).build())
        .build(),
))?;
queue.set_tracks(["https://example.com/a.mp3", "https://example.com/b.m3u8"])?;
let first = queue.tracks()[0].id;
queue.select(first, Transition::None)?;
queue.play();
```

`set_tracks` and `append` must run inside a tokio runtime. Without
`should_autoplay(true)` the queue waits for `select` before playing.
`append_at(source, position)` starts a track at a given position.

## Key Types

- `Queue` / `QueueConfig` — the deck and its builder: playback order, repeat,
  action at item end, crossfade, preload lead, concurrent loads.
- `QueueControl` — the handle `Host::insert` returns: add and remove tracks,
  `select`, `next`, `previous`, `seek`, `play`, `pause`, rate, volume.
- `TrackSource` — a URL, or a full `ResourceConfig` for headers, DRM keys, or
  format hints.
- `TrackId` — stable identity of a queue entry.
- `CrossfadeSettings` — duration, curve (linear or equal-power), and shape of
  a crossfade.
- `QueueEvent` — queue events, delivered on the same `EventBus` as player,
  audio, and network events.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-queue) for detailed contracts, invariants, and internals.
