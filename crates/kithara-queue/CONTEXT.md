# kithara-queue — Context

Detailed contracts and invariants for the kithara-queue crate; the README is the overview.

## Public API

- [`Queue::new(QueueConfig)`] constructs a queue-owned `PlayerImpl`.
- `Queue::build(Arc<PlayerImpl>, QueueConfig)` through
  `FromWithParams` decorates the supplied canonical player.
- CRUD: `append`, `insert(source, after)`, `remove`, `clear`,
  `set_tracks`
- Query: `tracks`, `track(id)`, `current`, `current_index`, `len`,
  `is_empty`
- Navigation: `select(id, transition)`, `advance_to_next(transition)`,
  `return_to_previous(transition)`,
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
  `ResourceConfig` via `ResourceConfig::new`.
- `TrackSource::Config(Box<ResourceConfig>)` — the caller provides a
  pre-built `ResourceConfig` (useful for DRM keys, custom headers,
  format hints). [`Queue`] leaves caller-set fields intact.

`From<&str>`, `From<String>`, `From<ResourceConfig>`, and
`From<Box<ResourceConfig>>` are implemented.

## Ownership And Player Composition

`QueueConfig` contains queue policy and dependencies only; it never stores a
player component. Construction is type-driven: `Queue::new` creates one player,
while `FromWithParams<Arc<PlayerImpl>, QueueConfig>` consumes the explicit
player argument. Both paths converge on the same private constructor and the
same queue state. There is no optional-player fallback or second player path.

`Queue` owns playlist identity, loading, navigation, pending selection,
auto-advance, and crossfade policy. Its `player` field is the canonical deck
handle. `Loader` holds clones of that same `Arc<PlayerImpl>`, `Arc<Tracks>`, and
`EventBus` only so asynchronous load completion can commit through the queue's
existing owners; those handles do not contain mirrored player, track, or event
state.

`Queue` implements the `kithara-play` `Player` and `PlayerComponent` contracts.
Registering it in a `MultiPlayer` consumes the queue, and the returned
`Member<Queue>` is the only caller control path. `PlayerComponent` contributes
the queue's one canonical `PlayerImpl` leaf to group tempo/seek validation;
`MultiPlayer` never reaches into queue navigation or loader state.

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

`Queue` is the sole auto-advance orchestrator: both construction paths call
`PlayerImpl::set_auto_advance_enabled(false)` to disable the player's built-in
linear handler, then drives transitions from player events and the host `tick()`
loop.

- on `PlayerEvent::HandoverRequested` (cf>0 only): `handle_handover_requested`
  calls `advance_loaded_successor(entry.id, Transition::Crossfade)`, which
  selects the next loaded entry through `select(next.id, transition)`.
  `Queue` does not consume `PrefetchRequested` and does not call
  `PlayerImpl::arm_next` / `commit_next`.
- on `ItemDidPlayToEnd`: after filtering stale fade-out signals, natural EOF
  advances through `advance_to_next(Transition::Crossfade)`; failures advance
  with `Transition::None`.
- during `tick()`: `maybe_arm_crossfade` checks the current
  `playback_view()`, `crossfade_duration`, and `should_arm_crossfade`; when the
  remaining time is inside the crossfade window, it pre-selects the loaded
  successor via `advance_loaded_successor`. The `crossfade_armed_for` marker
  prevents repeated selects for the same play-through and lets the later EOF
  signal be consumed instead of advancing twice.

Auto-advance is gated on the user pause state (`Queue::is_paused`, i.e. the
player's live rate is `0.0`): a paused queue never auto-advances or arms a
crossfade. The advance path resumes playback, so firing it while paused would
silently un-pause and let the committed position run on — the user paused, the
head must freeze. The event handlers (`ItemDidPlayToEnd`, `ItemDidFail`,
`handle_handover_requested`, `maybe_arm_crossfade`) therefore no-op while paused.
The gate reads the rate, not `is_playing()`: a natural end-of-track drops
`is_playing()` to `false` once the arena drains, and gating on that would wrongly
block the genuine end-of-track advance; the rate stays `> 0` through EOF and
only drops to `0.0` on a deliberate `pause`. Explicit user navigation (`select`,
`advance_to_next`, `play`) is unaffected — only the automatic transitions are
gated.

## Loading Lifecycle

Each `append` allocates a monotonic [`TrackId`] and a queue entry with
status `Pending`, then spawns a background load attempt:

1. Acquire a permit in the attempt's lane (see "Load Lanes" below).
2. Publish `TrackStatusChanged { Loading }`.
3. Build the `ResourceConfig` (either from `TrackSource::Uri` templates
   or the caller-supplied `Config`) and call `Resource::new`.
4. Spawn a LoadSlow listener on the config's `EventBus` — if
   `FileEvent::LoadSlow` or `HlsEvent::LoadSlow` fires,
   `TrackStatusChanged { Slow }` is published before completion.
5. On success: `PlayerImpl::replace_item(index, resource)`,
   `TrackStatusChanged { Loaded }`.
6. If the loaded track was stashed in `pending_select` (a `select(id, transition)`
   arrived before loading finished), complete the apply-after-load path through
   `select_item_with_crossfade`.
   Otherwise the track stays `Loaded` and does nothing until the caller
   explicitly selects it.

After `select_item` succeeds the engine has consumed `items[index]`, so
the Queue immediately transitions the entry to `TrackStatus::Consumed`.
Re-selecting a `Consumed` track respawns the load.

## Load Lanes

The loader runs two isolated permit lanes so a prefetch parked on a dead
host cannot starve a selection: **Prefetch** (append-time, capped by
`QueueConfig::max_concurrent_loads`) and **Interactive** (`select`, one
permit). Max in-flight is `max_concurrent_loads + 1`.

Each `TrackRecord` owns its track's status, source, and at most one live
attempt (`AttemptGuard`), all under the one `Tracks` lock. The guard
cancels the attempt's per-track `CancelToken` on drop, so removing the
record (`remove` / `clear`) or flipping the status to `Cancelled` aborts
the load with no separate call. `select` on a track still waiting for a
prefetch permit promotes it into the interactive lane (the parked
attempt is replaced and abandons on wake via a generation check); an
attempt already downloading is left alone. A cancelled attempt returns
`QueueError::Cancelled` and leaves `TrackStatus` to the superseding
path. Byte-level dedupe of same-URL downloads is the `AssetStore`'s job.

The production append/insert path does not start initial playback on its own:
the caller drives the first `select` / `play` explicitly so playback order is
deterministic and independent of which load finishes first.
`QueueConfig::should_autoplay` defaults to `true`, but its current consumption is
cfg-gated to the test/probe harness (`register_for_test` /
`complete_load_for_test`), not the production loader path.

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

## Advance Commit Ownership

`Queue::advance_to_next` resolves the next selectable entry from a read-only
navigation snapshot. It must not mutate `NavigationState` before the player
selection commits. A `Loaded` entry commits synchronously inside `select`; a
`Pending` / `Loading` / `Consumed` entry commits later in
`spawn_apply_after_load` after the resource has been planted and
`select_item_with_crossfade` succeeds. If navigation moves before that commit,
repeated EOF / handover notifications can run ahead of the audible player and
exhaust the queue while the player still points at the old track.

After `QueueEnded`, a later `Queue::seek` reparks navigation from the last
navigation-owned index, not from `PlayerImpl::current_index`. The queue owns item
identity; the player cursor is only an engine slot cursor and may be stale after
EOF/drain.

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
| `player.select_item(idx, true)`              | `queue.select(id, transition)`                    |
| `player.tick()`                              | `queue.tick()`                                    |

DRM stays in the caller. `kithara-queue` is DRM-agnostic; apps that
need zvuk DRM keys build a `ResourceConfig::new(url).with_keys(..)` and
pass it via `TrackSource::Config(Box::new(cfg))`. See
`kithara-app::sources::build_source` for a reference implementation.
