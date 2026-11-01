<div align="center">

<img src="https://raw.githubusercontent.com/zvuk/kithara/main/logo.svg" alt="kithara" width="300">

</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-queue.svg)](https://crates.io/crates/kithara-queue)
[![docs.rs](https://docs.rs/kithara-queue/badge.svg)](https://docs.rs/kithara-queue)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/zvuk/kithara/blob/main/LICENSE-MIT)

</div>

# kithara-queue

AVQueuePlayer-analogue orchestration layer on top of `kithara-play`. Owns
the queue (ordered tracks), an async track loader with a configurable
parallelism cap, navigation (shuffle / repeat / history), and
crossfade-aware track selection. Replaces the bespoke queue / controller
code previously duplicated across `kithara-app` and future iOS / Android
SDK surfaces.

## Usage

```rust
use kithara_bufpool::{OverallBudget, PoolConfig, PoolError, pool_schema};
use kithara_platform::sync::Arc;
use kithara_play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl};
use kithara_queue::{Queue, QueueConfig, Transition};

pool_schema! {
    pub AppPools {
        bytes: u8,
        samples: f32,
    }
}

#[tokio::main]
async fn main() -> Result<(), PoolError> {
    let config = || PoolConfig::builder().max_buffers(128).build();
    let pools = AppPools::builder(OverallBudget(64 * 1024 * 1024))
        .bytes(config())
        .samples(config())
        .build()?;
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let player = PlayerImpl::new(
        PlayerConfig::builder().worker(worker).build(),
    );
    let queue = Arc::new(Queue::new(
        QueueConfig::builder().player(player).build(),
    ));
    queue.set_tracks(["https://example.com/a.mp3", "https://example.com/b.mp3"]);

    // Caller explicitly picks the first track to play. Queue autoplays
    // only when built with `should_autoplay(true)`; otherwise the UI (or
    // any other caller) calls `select` / `play` when the user is ready.
    if let Some(first) = queue.tracks().first() {
        let _ = queue.select(first.id, Transition::None);
    }

    let mut rx = queue.subscribe();
    while let Ok(event) = rx.recv().await {
        println!("{event:?}");
    }
    Ok(())
}
```

`Queue<S>`, `QueueConfig<S>`, and `TrackSource<S>` require the schema to
provide both `HasPool<u8>` and `HasPool<f32>`. The queue never constructs a
pool region. When no store is supplied, it builds the store from the exact
`PoolRegion<S>` already owned by the player, preserving the shared hard budget.

`Queue::set_tracks` must run inside an active tokio runtime because the
loader uses `tokio::spawn`.

Use `QueueControl::append_at(source, Position::from_secs(32))` to load an item
at an initial media position, including background preload. The entry keeps
that position for fresh loads; seeking or resuming an already opened track
continues to use its current playback position.

`QueueConfig` owns the initial playback order, item-end action, and complete
crossfade profile. Their runtime setters update that same queue-owned state;
repeat remains a separate automatic-EOF rule, and `should_autoplay` applies
only to the first load.

## Key Types

`QueueControl::eq_band(effect, band)` returns a Config control bound to the loaded
track. Import `kithara_play::EqBandConfigControl` for generated gain setters, or
use `Configure` with `When<SessionFrame>` for a timed step. Retired handles refuse
writes and retain their last confirmed settings.

[`Queue<S>`] owns a `PlayerImpl<S>` (from `kithara-play`) and composes it with:

- an ordered `Vec<TrackEntry>` indexed by stable [`TrackId`]s,
- an async [`Loader`] (internal) that caps in-flight `Resource::new`
  calls via a `tokio::sync::Semaphore`,
- [`NavigationState`] for stable-ID sequential or shuffle traversal, repeat,
  and actual selection history,
- a `pending_select` slot so `Queue::select(id)` can be called before the
  track has finished loading.

[`Queue`] emits [`QueueEvent`] on the shared `EventBus` from
`kithara-events`, so subscribers receive queue-level signals and the
underlying player / audio / hls / file events through a single stream.

- [`Queue::new(QueueConfig)`] - the orchestrator, generic over the player's
  registered pool schema: CRUD (`append`,
  `insert`, `remove`, `clear`, `set_tracks`), navigation (`select`,
  `next`, `previous`, typed playback order / repeat / item-end action / `seek`),
  playback controls delegated to `PlayerImpl`, and `tick()` to drive the
  player and drain engine events.
- [`CrossfadeSettings`] — duration, linear or equal-power curve, blend depth,
  and temporal crossover pivot. Linear controls amplitude and can dip perceived
  power for unrelated tracks; equal-power approximately preserves uncorrelated
  power and can raise correlated material.
- [`TrackSource<S>`] - input to `append` / `insert` / `set_tracks`, either a
  `Uri(String)` (Queue builds a default `ResourceConfig`) or a
  `Config(Box<ResourceConfig>)` (caller-built, for DRM keys / headers /
  format hints).
- [`QueueEvent`] — queue-level signals delivered via [`Queue::subscribe`]
  alongside the underlying player / audio / hls / file events.

See [crate contracts](https://github.com/zvuk/kithara/wiki/kithara-queue) for detailed contracts, invariants, and internals.
