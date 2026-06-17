<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-queue.svg)](https://crates.io/crates/kithara-queue)
[![docs.rs](https://docs.rs/kithara-queue/badge.svg)](https://docs.rs/kithara-queue)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-queue

AVQueuePlayer-analogue orchestration layer on top of `kithara-play`. Owns
the queue (ordered tracks), an async track loader with a configurable
parallelism cap, navigation (shuffle / repeat / history), and
crossfade-aware track selection. Replaces the bespoke queue / controller
code previously duplicated across `kithara-app` and future iOS / Android
SDK surfaces.

## Overview

[`Queue`] composes an `Arc<PlayerImpl>` (from `kithara-play`) with:

- an ordered `Vec<TrackEntry>` indexed by stable [`TrackId`]s,
- an async [`Loader`] (internal) that caps in-flight `Resource::new`
  calls via a `tokio::sync::Semaphore`,
- [`NavigationState`] for shuffle / repeat / history,
- a `pending_select` slot so `Queue::select(id)` can be called before the
  track has finished loading.

[`Queue`] emits [`QueueEvent`] on the shared `EventBus` from
`kithara-events`, so subscribers receive queue-level signals and the
underlying player / audio / hls / file events through a single stream.

## Key Types

- [`Queue::new(QueueConfig)`] — the orchestrator: CRUD (`append`,
  `insert`, `remove`, `clear`, `set_tracks`), navigation (`select`,
  `advance_to_next`, `return_to_previous`, shuffle / repeat / `seek`),
  playback controls delegated to `PlayerImpl`, and `tick()` to drive the
  player and drain engine events.
- [`TrackSource`] — input to `append` / `insert` / `set_tracks`, either a
  `Uri(String)` (Queue builds a default `ResourceConfig`) or a
  `Config(Box<ResourceConfig>)` (caller-built, for DRM keys / headers /
  format hints). `From<&str>` / `String` / `ResourceConfig` /
  `Box<ResourceConfig>` are implemented.
- [`QueueEvent`] — queue-level signals delivered via [`Queue::subscribe`]
  alongside the underlying player / audio / hls / file events.

## Minimal Usage

```rust
use std::sync::Arc;

use kithara_queue::{Queue, QueueConfig, Transition};

#[tokio::main]
async fn main() {
    let queue = Arc::new(Queue::new(QueueConfig::new()));
    queue.set_tracks(["https://example.com/a.mp3", "https://example.com/b.mp3"]);

    // Caller explicitly picks the first track to play. Queue does not
    // autoplay on its own — the UI (or any other caller) is responsible
    // for calling `select` / `play` when the user is ready.
    if let Some(first) = queue.tracks().first() {
        let _ = queue.select(first.id, Transition::None);
    }

    let mut rx = queue.subscribe();
    while let Ok(event) = rx.recv().await {
        println!("{event:?}");
    }
}
```

`Queue::set_tracks` must run inside an active tokio runtime because the
loader uses `tokio::spawn`.

See [CONTEXT.md](CONTEXT.md) for detailed contracts, invariants, and internals.
