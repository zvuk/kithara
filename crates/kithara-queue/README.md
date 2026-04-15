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

## Public API

- [`Queue::new(QueueConfig)`] / [`Queue::wrap(Arc<PlayerImpl>, QueueConfig)`]
- CRUD: `append`, `insert(source, after)`, `remove`, `clear`,
  `set_tracks`
- Query: `tracks`, `track(id)`, `current`, `current_index`, `len`,
  `is_empty`
- Navigation: `select(id)`, `advance_to_next`, `return_to_previous`,
  `set_shuffle` / `is_shuffle_enabled`, `set_repeat` / `repeat_mode`,
  `seek(seconds)`
- Delegated to `PlayerImpl`: `play`, `pause`, `is_playing`,
  `crossfade_duration` / `set_crossfade_duration`, `default_rate` /
  `set_default_rate`, `volume` / `set_volume`, `is_muted` / `set_muted`,
  `eq_band_count`, `eq_gain`, `set_eq_gain`, `reset_eq`,
  `position_seconds`, `duration_seconds`
- Lifecycle: `tick()` — call from the host loop to drive
  `PlayerImpl::tick` and drain engine events into `QueueEvent`s.

[`TrackSource`] is the input to `append` / `insert` / `set_tracks`. It
has two shapes:

- `TrackSource::Uri(String)` — the [`Queue`] builds a default
  `ResourceConfig` from the `QueueConfig` `net` / `store` templates.
- `TrackSource::Config(Box<ResourceConfig>)` — the caller provides a
  pre-built `ResourceConfig` (useful for DRM keys, custom headers,
  format hints). [`Queue`] leaves caller-set fields intact.

`From<&str>`, `From<String>`, `From<ResourceConfig>`, and
`From<Box<ResourceConfig>>` are implemented.

## Event Flow

[`Queue::subscribe`] returns an `EventReceiver` that sees everything
published on the underlying [`EventBus`]: `Event::Queue(QueueEvent::..)`
plus `Event::Player(..)`, `Event::Audio(..)`, `Event::Hls(..)`, and
`Event::File(..)`.

[`QueueEvent`] variants:

- `TrackAdded { id, index }`
- `TrackRemoved { id }`
- `TrackStatusChanged { id, status }` — `Pending` → `Loading` / `Slow`
  → `Loaded` → `Consumed` (after the engine takes the `Resource`
  during `select_item`) → `Failed(reason)` on error
- `CurrentTrackChanged { id }` — forwarded from
  `PlayerEvent::CurrentItemChanged`
- `QueueEnded` — emitted by `advance_to_next` when navigation returns
  `None` and [`RepeatMode::Off`] is active
- `CrossfadeStarted { duration_seconds }` — emitted when the engine is
  about to fade from a playing track to the newly-selected one
- `CrossfadeDurationChanged { seconds }`

## Loading Lifecycle

Each `append` allocates a monotonic [`TrackId`] and a queue entry with
status `Pending`, then spawns a background task:

1. Acquire a semaphore permit (up to
   `QueueConfig::max_concurrent_loads`).
2. Publish `TrackStatusChanged { Loading }`.
3. Build the `ResourceConfig` (either from `TrackSource::Uri` templates
   or the caller-supplied `Config`) and call `Resource::new`.
4. Spawn a LoadSlow listener on the config's `EventBus` — if
   `FileEvent::LoadSlow` or `HlsEvent::LoadSlow` fires,
   `TrackStatusChanged { Slow }` is published before completion.
5. On success: `PlayerImpl::replace_item(index, resource)`,
   `TrackStatusChanged { Loaded }`.
6. If the loaded track was stashed in `pending_select` (a `select(id)`
   arrived before loading finished), call `select_item(index, true)`.
7. Otherwise, if `autoplay` is on and the player is idle, call
   `select_item(index, true)`.

After `select_item` succeeds the engine has consumed `items[index]`, so
the Queue immediately transitions the entry to `TrackStatus::Consumed`.
Re-selecting a `Consumed` track respawns the load.

## Minimal Usage

```rust
use std::sync::Arc;

use kithara_play::PlayerConfig;
use kithara_queue::{Queue, QueueConfig};

#[tokio::main]
async fn main() {
    let mut config = QueueConfig::new(PlayerConfig::default());
    config.autoplay = true;

    let queue = Arc::new(Queue::new(config));
    queue.set_tracks(["https://example.com/a.mp3", "https://example.com/b.mp3"]);

    let mut rx = queue.subscribe();
    while let Ok(event) = rx.recv().await {
        println!("{event:?}");
    }
}
```

`Queue::set_tracks` must run inside an active tokio runtime because the
loader uses `tokio::spawn`.

## Migration From kithara-app

The previous `kithara-app::{playlist, controls}` combination collapses
into a single [`Queue`]:

| Old (`kithara-app`)                          | New (`kithara-queue`)                             |
|----------------------------------------------|---------------------------------------------------|
| `Arc<PlayerImpl>` + `Arc<Playlist>` + `AppController` + `TrackLoadParams` | `Arc<Queue>` |
| `AppController::load_params.load_and_apply` | `queue.set_tracks(sources)` / `queue.append(src)` |
| `playlist.track_name(i)`                     | `queue.tracks()[i].name`                          |
| `playlist.track_status(i)`                   | `queue.tracks()[i].status`                        |
| `playlist.get_next_track()` + switch         | `queue.advance_to_next()`                         |
| `playlist.get_prev_track()` + switch         | `queue.return_to_previous()`                      |
| `player.seek_seconds(s)`                     | `queue.seek(s)`                                   |
| `player.select_item(idx, true)`              | `queue.select(id)`                                |
| `player.tick()`                              | `queue.tick()`                                    |

DRM stays in the caller. `kithara-queue` is DRM-agnostic; apps that
need zvuk DRM keys build a `ResourceConfig::new(url).with_keys(..)` and
pass it via `TrackSource::Config(Box::new(cfg))`. See
`kithara-app::sources::build_source` for a reference implementation.
