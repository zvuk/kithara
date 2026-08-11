# kithara-queue — Context

Contracts and invariants for `kithara-queue`; the README is the overview.

## Ownership

- `Queue` owns the player's item list. A caller-supplied `PlayerImpl` may still be
  driven for `play` / `pause` / `seek`, but `replace_item`, `clear_item`,
  `reserve_slots`, `select_item*`, `remove_at`, `remove_all_items` are Queue-owned.
- `Tracks` is the sole owner of `Vec<TrackRecord>` (status, source, live load
  attempt), shared with `Loader` through `Arc<Tracks>`. Every status transition MUST
  go through `Tracks::set_status` (or the attempt ops) so the polled view and the
  `QueueEvent::TrackStatusChanged` stream cannot drift.
- `NavigationState` owns item identity; `PlayerImpl::current_index` is only an engine
  slot cursor and may be stale after EOF / drain.
- `Queue::new` calls `PlayerImpl::set_auto_advance_enabled(false)`: the queue is the
  sole auto-advance orchestrator.

## Construction and config

`Queue::new(QueueConfig)`; `QueueConfig` is a `bon` builder (`QueueConfig::builder()`,
`Default` = `builder().build()`).

- `cancel`: `Some` threads the app master so the queue subtree cascades from one
  owner; `None` falls back to a fresh standalone root (test / library use only, never
  the production app path). `Queue::drop` cancels it. A queue-built player gets this
  token as `PlayerConfig::cancel`; a caller-supplied player owns its own master.
- `store`: `None` builds an `AssetStore` on the player's byte pool.
  `TrackSource::Uri` resources share it; a caller-supplied `ResourceConfig` keeps its
  own.
- `max_concurrent_loads` (default 3) sizes the prefetch lane only.
- `prefetch_duration` (default 3.5) is declared but not applied by `Queue::new`.
- `should_autoplay` (default `true`) is consumed only by the
  `cfg(any(test, feature = "probe"))` harness. The production append / insert path
  never starts playback: the caller drives the first `select` / `play`, so order is
  deterministic and independent of which load finishes first.

## Track sources

- `TrackSource::Uri`: the loader parses with `ResourceConfig::parse_src`, then
  `ResourceConfig::for_src(src).store(queue store).byte_pool(..).pcm_pool(..)`.
  `TrackSource::Config(Box<ResourceConfig>)` passes through untouched (DRM keys,
  headers, format hints preserved). Both then run `PlayerImpl::prepare_config`; a
  config with no bus gets the player bus `scoped_labeled` with the track id. An
  attempt requires `config.cancel()` to be `Some`; a missing per-track cancel fails
  the load with `QueueError::Resource`.
- `TrackSource` is `Clone`, so a `Consumed` / `Cancelled` / `Failed` track can be
  respawned without the caller rebuilding anything. `track_source(id)` resolves by
  identity, not by index.
- DRM stays in the caller; the crate is DRM-agnostic (see
  `kithara-app::sources::build_source` for building a keyed config).

## Event flow

`Queue::subscribe` returns a receiver on the shared `EventBus`: `Event::Queue` plus the
underlying player / audio / hls / file / downloader events.

- `CurrentTrackChanged { id: Option<TrackId> }` mirrors
  `PlayerEvent::CurrentItemChanged`, re-announced by `seek` after queue end.
- `CurrentTrackAdvance { id, reason }` is published wherever the queue itself commits
  a selection, carrying the `AdvanceReason`.
- `NextTrackReady { id, index }` fires when a load lands in a still-valid slot.
- `AudioEvent::UnderrunStarted` / `UnderrunEnded` translate to
  `ItemEvent::PlaybackStalled` / `PlaybackLikelyToKeepUp`.
- `player_rx` (`drain_player_events`, `engine_events.rs`) shares the root `EventBus`
  channel with every descendant scope, so under contention it can lag and lose
  `PlayerEvent::CurrentItemChanged` — edge-triggered and de-duplicated at the source
  (`ItemQueue::announce_current_item`), so a dropped copy never re-arrives on its own.
  On `TryRecvError::Lagged`, `drain_player_events` finishes draining, releases the
  receiver lock, then calls `handle_current_item_changed()` once to resync from
  `PlayerImpl::current_index()` directly — the same resync `seek` already performs
  after an indeterminate gap. The resync runs after the lock is released so its own
  publish cannot evict the next unread slot and re-trigger `Lagged` against itself.

## Status lifecycle and load lanes

`Pending` → `Loading` (lane permit won) → `Slow` (on `DownloaderEvent::LoadSlow`) →
`Loaded` (after `replace_item`) → `Consumed` (the engine took the resource).
`Failed(reason)` on error; `Cancelled` when a later `select` supersedes an in-flight
load. Selecting a `Consumed` / `Cancelled` / `Failed` track respawns a load in the
interactive lane.

Two isolated semaphores so a prefetch parked on a dead host cannot starve a selection:
**Prefetch** (append-time, `max_concurrent_loads` permits) and **Interactive**
(`select`, one permit). Max in-flight is `max_concurrent_loads + 1`.

- One live attempt per track (`AttemptGuard` in `TrackRecord`, under the single
  `Tracks` lock). Dropping the guard armed cancels the per-track `CancelToken`, so
  removing the record (`remove` / `clear`) or flipping the status to `Cancelled`
  aborts the load with no separate call. `finish_attempt` disarms — the token then
  belongs to the built `Resource`.
- Tickets are generation-checked: a replaced ticket loses its claim (`mark_loading`
  returns `false`, the task releases its permit and returns `QueueError::Cancelled`).
- `select` on a track still waiting for a prefetch permit promotes it into the
  interactive lane; `promote_attempt` replaces a waiting (or cancelled but still
  unwinding) attempt, keeps one that already holds a permit, and declines a vacant
  slot because the completion path then owns what happens next.
- A cancelled attempt returns `QueueError::Cancelled` and leaves `TrackStatus` to the
  superseding path. Byte-level dedupe of same-URL downloads is the `AssetStore`'s job.

## Selection serialization

`Queue::select_with_reason` and the post-load apply in `watch_apply` mutate the same
selection state (`pending_select`, navigation cursor, current item, the `Cancelled`
supersede marker). The `select_apply` mutex serialises them, held only across each
side's **synchronous** critical section — never across `.await`; without it a
completion could observe "not cancelled", consume `pending_select`, then `select_item`
after a later `select` already committed.

Superseding a still-loading selection marks the prior pending track `Cancelled`
(`override_pending_select` / `cancel_stale_pending`) and evicts its player slot with
`clear_item`. The completion path reads that marker and skips its `select_item`; the
eviction closes the race where a fast loader planted the resource *before* the
override ran. Pinned by `tests/tests/kithara_queue/track_switch_race.rs`. Re-selecting
the already-playing track is a no-op apart from dropping stale pending state.

## Beat-grid starts

`Queue::start_at_beat` owns the cold-deck SYNC path: it binds first, waits for
the resource to land, then arms the stamped start. A current deck with a
positive playback rate is rejected as `QueueError::NotReady(track_id)` before
any binding is published. The GUI gesture has no in-flight phase-handoff
contract, so it must not turn a running track into a decode failure or queue
advance.

## Advance and auto-advance

`advance_to_next` resolves the next entry from a read-only navigation snapshot
(`next_selectable_entry` skips `Cancelled` records) and must not mutate
`NavigationState` before the player selection commits: a `Loaded` entry commits
synchronously inside `select`, a `Pending` / `Loading` / `Slow` / `Consumed` entry
commits later in the `watch_apply` completion, after the resource is planted and
`select_item_with_crossfade` succeeds. Moving navigation early lets repeated EOF /
handover notifications run ahead of the audible player and exhaust the queue.

After `QueueEnded`, `Queue::seek` re-parks navigation from
`NavigationState::last_selected_index` and re-announces `CurrentTrackChanged` before
seeking — not from `PlayerImpl::current_index`.

- `HandoverRequested` → `advance_loaded_successor`, which selects the successor only
  if it is already `Loaded`. The queue never consumes `PrefetchRequested` and never
  calls `arm_next` / `commit_next`.
- `ItemDidPlayToEnd`: a non-empty `src` is the authoritative natural-EOF signal →
  `advance_to_next(Crossfade, NaturalEof)`. An empty `src` falls back to the pos/dur
  heuristic (advance when `pos >= dur - 1.0s`, else log a spurious crossfade fade-out).
- `ItemDidFail` → status `Failed`, `TrackLoadFailed { auto_skipped: true }`, then
  `advance_to_next(Transition::None, TrackFailed)`.
- Both handlers publish `QueueEnded` when `current()` is `None`: a stale EOF after
  queue end must not restart from the first track.
- `tick()` → `maybe_arm_crossfade`: `should_arm_crossfade` requires `crossfade > 0`,
  positive position and duration, remaining time inside the crossfade window, and no
  existing arm for this track. `crossfade_armed_for` is recorded only when the
  player's `current_index` actually moved; the later EOF for that track is then
  consumed (`consume_armed_advance`) instead of advancing twice. Cleared on
  `CurrentTrackChanged`.
- Pause gate: every automatic path no-ops while `is_paused()`
  (`PlayerImpl::rate() <= 0.0`). It reads the rate, not `is_playing()`, which a
  natural end-of-track drops once the arena drains — that would wrongly block the
  genuine advance, while the rate only reaches `0.0` on a deliberate `pause`. Explicit
  `select` / `advance_to_next` / `return_to_previous` / `play` are never gated.

## Position, playback view, and play()

- `cached_position` is refreshed each `tick`; a `0.0` sample is dropped when the
  previous position was above 0.5 s (transient blip on pause/resume). `pause()`
  freezes it, `CurrentItemChanged` resets it to `Unknown`, a landed seek writes
  `landed_at`. `position_seconds()` reads this cache, not the engine.
- `playback_view()` is one coherent read: duration `0.0` collapses to `None`,
  `buffered = max(frontier, cached)`, `position` replaced by the cached value. The
  union is deliberate — the cached span is what a host progress bar means by
  "available without more network", and the decoded frontier stays a floor because a
  window behind the playhead deadlocks the host into buffering.
- No queue-level seek watchdog: the audio pipeline's `#[hang_watchdog]` already panics
  with context on a stalled seek.
- `play()` starts the engine, then under `select_apply` reads back from the player
  which slot was consumed (`item_has_resource`) rather than inferring it from a
  beforehand status snapshot — a load can complete inside the engine-start window.
  Slot filled by a `Loaded` track → mark `Consumed` (a stale `Loaded` over an emptied
  slot makes every later select fail with `PlayError::ItemConsumed`). Slot still empty
  with the track `Pending` / `Loading` / `Slow` → record a pending select and promote
  the load, so `watch_apply` applies the intent when the resource lands; without this
  the (normal) case where `play()` wins the race stays silent forever.

## Removal, ids, and navigation

- `remove(id)` on the current track switches to the next entry — or the previous one
  at the tail — with `Transition::None` and `AdvanceReason::RemovedCurrent`; with
  nothing left it pauses the player. Dropping the record aborts its load.
- `clear()` drops all records (aborting loads), calls `remove_all_items()`, then
  publishes `TrackRemoved` per id.
- `TrackId::allocate()` is a process-wide monotonic counter. `append_with_id` /
  `insert_with_id` accept a caller-owned id, which MUST come from `TrackId::allocate`
  so it stays in that address space (FFI reserves the id at item construction and
  surfaces it as `audioId`).
- `NavigationState` is pure logic; the caller owns locking. History is deduped against
  its tail and capped at 100 entries. `next()`: unselected → `0`; `RepeatMode::One` →
  current; `All` wraps to `0`; `Off` returns `None` and clears the current index at the
  end. `prev()` returns `None` at index 0 or before the first selection. `finish()`
  pushes the current index into history and clears it, keeping `last_selected_index()`.
  `shuffle_enabled` is stored and reported but not consulted by `next()` / `prev()`.

## Test-only surface

`register_for_test`, `complete_load_for_test`, `insert_loaded_for_test`, and
`supply_test_resource_for_respawn` are gated behind
`cfg(any(test, feature = "probe"))`. `select` on a re-selectable track plants a
pre-supplied respawn resource synchronously, falling through to the real loader when
none is registered.
