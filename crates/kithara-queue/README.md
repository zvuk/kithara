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
use kithara_bufpool::{OverallBudget, Pool, PoolConfig, PoolSlot};
use kithara_host::{Host, HostConfig};
use kithara_play::{PlayWorker, PlayWorkerConfig, ResourcePrep};
use kithara_queue::{Queue, QueueConfig, Transition};

#[derive(Pool)]
pub struct AppPools {
    bytes: PoolSlot<u8>,
    samples: PoolSlot<f32>,
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

`replace_source(current_id, source)` prepares another source for the current
track, retaining its identity, metadata, position and play/pause state. The
accepted URL changes when the replacement enters the deck. Preparation failure
keeps the accepted resource. A seek cancels preparation; during a submitted
handover it returns `NotReady` until that handover settles.

`next` starts the prepared automatic successor without waiting for track end.
An accepted manual target remains the cursor for later `next` calls.

The queue is the one reader of analysis documents. A track whose
`ResourceConfig` carries an `analysis` request is read as it is added, under a
child of the queue's master cancel, through the track's asset store and
downloader; no load or play waits for it, and loads and replays never read it
again. `TrackEntry::analysis()` states the read: not requested, loading, ready
with the analysis, or failed, and each change is announced as
`QueueEvent::TrackAnalysisChanged`. A failed read stays failed and does not
touch playback. Removing the track cancels its read; a committed replacement
source with another request cancels the old read and starts its own, and its
old answer is dropped; the same request keeps the state.

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
