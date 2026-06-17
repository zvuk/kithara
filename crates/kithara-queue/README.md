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

## Public API

- [`Queue::new(QueueConfig)`]
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

## Auto-Advance Contract

`Queue` is the sole auto-advance orchestrator: `Queue::new` calls
`PlayerImpl::set_auto_advance_enabled(false)` to disable the player's
built-in linear handler, then drives transitions from
`PlayerEvent::PrefetchRequested` / `HandoverRequested`:

- on `PrefetchRequested`: resolve the next index via
  `NavigationState::peek_next` (honouring shuffle / repeat); if the
  resolved entry is `TrackStatus::Loaded`, call `arm_next(idx)`. Tracks
  that are still loading are picked up via the
  `TrackStatusChanged { Loaded }` retry path.
- on `HandoverRequested` (cf>0 only): call `commit_next(idx)`,
  advance navigation, mark the just-promoted track `Consumed`, publish
  `QueueEvent::CrossfadeStarted`.
- on `ItemDidPlayToEnd`: the audio thread already advanced (cf=0 arena
  handover) or the queue did (cf>0 commit). `sync_navigation_after_handover`
  brings `NavigationState::current_index` in line with the player and
  emits `QueueEvent::QueueEnded` if no further track is reachable.

Auto-advance is gated on the user pause state (`Queue::is_paused`, i.e.
the player's live rate is `0.0`): a paused queue never auto-advances or
arms a crossfade. The advance path resumes playback (`autoplay: true`),
so firing it while paused would silently un-pause and let the committed
position run on — the user paused, the head must freeze. The event
handlers (`ItemDidPlayToEnd`, `ItemDidFail`, `maybe_arm_crossfade`)
therefore no-op while paused. The gate reads the rate, not
`is_playing()`: a natural end-of-track drops `is_playing()` to `false`
once the arena drains, and gating on that would wrongly block the
genuine end-of-track advance; the rate stays `> 0` through EOF and only
drops to `0.0` on a deliberate `pause`. Explicit user navigation
(`select`, `advance_to_next`, `play`) is unaffected — only the
automatic, event-driven transitions are gated.

`set_repeat`, `set_shuffle`, `Queue::remove`, and `Queue::clear` call
`PlayerImpl::unarm_next` so a stale arm cannot survive a navigation /
queue mutation. The previous `Queue::tick`-based polling
(`maybe_arm_crossfade`, `should_arm_crossfade`) is removed; `tick` now
only ticks the player and drains events.

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
   Otherwise the track stays `Loaded` and does nothing until the caller
   explicitly selects it.

After `select_item` succeeds the engine has consumed `items[index]`, so
the Queue immediately transitions the entry to `TrackStatus::Consumed`.
Re-selecting a `Consumed` track respawns the load.

The Queue never starts playback on its own: there is no autoplay. The
caller drives the first `select` / `play` explicitly so playback order
is deterministic and independent of which load finishes first.

## Selection serialization

`Queue::select` and a track's `spawn_apply_after_load` completion both mutate the
same selection state — `pending_select`, the navigation cursor, the current item,
and the `TrackStatus::Cancelled` supersede marker. They are serialized by an
internal `select_apply` lock, held only across each side's **synchronous**
critical section (never across an `.await`).

This closes a barge-in race: superseding a still-loading selection works by
marking the prior pending track `Cancelled` (`override_pending_select` /
`cancel_stale_pending`), which the completion path reads to skip its
`select_item`. Without serialization a completion could observe "not cancelled",
consume `pending_select`, and then run its `select_item` *after* a later
`select` had already committed — letting the superseded track barge in over the
new current. The lock makes the completion's cancelled-check and `select_item` a
single critical section, mutually exclusive with `select`. Pinned by
`tests/.../track_switch_race.rs`.

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
