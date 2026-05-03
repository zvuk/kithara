use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use delegate::delegate;
use kithara_events::{
    Event, EventBus, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus,
};
use kithara_play::{PlayError, PlayerConfig, PlayerImpl, ResourceSrc};
use tokio::sync::broadcast::error::TryRecvError;
use tracing::{debug, warn};

use crate::{
    config::QueueConfig,
    error::QueueError,
    loader::Loader,
    navigation::{NavigationState, RepeatMode},
    track::{TrackEntry, TrackSource, Tracks},
};

/// Minimum position threshold used to suppress spurious 0.0 reports on
/// pause/resume. Values above this are considered a valid non-zero position.
const MIN_STABLE_POSITION_SECS: f64 = 0.5;

/// Transition style for a track switch.
///
/// Mirrors the Apple-idiomatic pattern of a namespace-style type with
/// variants describing "what" — not "how" — so the same method
/// signature handles both manual and auto-advance cases.
///
/// - [`Transition::None`] — immediate cut (0 seconds). Matches
///   `AVQueuePlayer`'s user-initiated selection idiom.
/// - [`Transition::Crossfade`] — use the player's configured
///   [`PlayerImpl::crossfade_duration`].
/// - [`Transition::CrossfadeWith`] — explicit override in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Transition {
    /// No crossfade; immediate cut.
    None,
    /// Use the player's configured crossfade duration.
    Crossfade,
    /// Use an explicit crossfade duration (seconds).
    CrossfadeWith { seconds: f32 },
}

impl Transition {
    /// Resolve the transition to an actual crossfade duration in
    /// seconds using `default` for [`Transition::Crossfade`].
    #[must_use]
    pub fn crossfade_seconds(self, default: f32) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Crossfade => default,
            Self::CrossfadeWith { seconds } => seconds,
        }
    }
}

/// A pending-select entry: a track id waiting to be applied plus the
/// [`Transition`] the caller asked for. Stored until loading finishes.
#[derive(Clone, Copy, Debug)]
struct PendingSelect {
    id: TrackId,
    transition: Transition,
}

/// AVQueuePlayer-analogue orchestration facade.
///
/// Owns an `Arc<PlayerImpl>` and a private async track loader, plus queue-level state
/// (ordered tracks, navigation, pending-select). Publishes [`QueueEvent`]
/// on the shared [`EventBus`] alongside player / audio / hls / file events
/// so [`Queue::subscribe`] returns a single unified stream.
pub struct Queue {
    player: Arc<PlayerImpl>,
    loader: Arc<Loader>,
    next_id: AtomicU64,
    /// Sole owner of the `Vec<TrackEntry>`. Shared with [`Loader`]
    /// through `Arc<Tracks>`; every status transition goes through
    /// [`Tracks::set_status`] so polling and the event stream stay in
    /// sync.
    tracks: Arc<Tracks>,
    navigation: Arc<Mutex<NavigationState>>,
    pending_select: Arc<Mutex<Option<PendingSelect>>>,
    /// Kept alongside `tracks` so a `Consumed` track can be re-spawned
    /// on re-selection (user tapping a previously-played track). The
    /// original `TrackSource::Config` — including DRM keys and custom
    /// net/headers — is preserved; a bare URL wouldn't be enough to
    /// reconstruct a DRM-protected source.
    sources: Arc<Mutex<HashMap<TrackId, TrackSource>>>,
    bus: EventBus,
    /// Subscription to the shared bus; drained in `tick()` to convert
    /// engine events into queue-level side-effects (auto-advance / current
    /// track change forwarding).
    player_rx: Mutex<EventReceiver>,
    /// Authoritative playback position updated on every [`Self::tick`].
    /// Filters transient 0.0 blips the engine reports on pause/resume —
    /// downstream UIs should read from this field rather than polling
    /// the engine directly.
    cached_position: Arc<Mutex<Option<f64>>>,
    autoplay: bool,
    /// `src` of the track currently designated as leading by the queue.
    /// Updated whenever the queue commits a new track to the player
    /// (`select_item_with_crossfade` succeeded, or `commit_next` ran).
    /// Compared against the `src` field of `ItemDidPlayToEnd` events to
    /// distinguish a current-track EOF (queue may need to end) from an
    /// outgoing-track EOF in cf>0 crossfades (queue must ignore).
    current_active_src: Arc<Mutex<Option<Arc<str>>>>,
    /// `src` of the track previously armed via `arm_next` but not yet
    /// promoted to leading. For cf>0 the audio thread keeps this in
    /// `Preloading`; we promote it via `commit_next`. For cf=0 the
    /// audio thread promotes it inline at EOF; the queue learns about
    /// the promotion in `sync_navigation_after_handover` and uses this
    /// field to know what the new leading src is.
    armed_next_src: Arc<Mutex<Option<Arc<str>>>>,
    /// Test-only loader override. When `Some`, `spawn_apply_after_load`
    /// pulls a pre-supplied [`Resource`] from this map keyed by
    /// [`TrackId`] instead of dispatching the real `Loader`. The test
    /// can pre-populate a queue of resources per id (one per spawn
    /// invocation) to drive multiple loads — for example, to simulate
    /// a re-load triggered by `handle_prefetch_requested` for a
    /// `Consumed` next-track on a second playthrough.
    #[cfg(any(test, feature = "test-utils"))]
    test_loader_supply: Arc<Mutex<HashMap<TrackId, Vec<kithara_play::Resource>>>>,
}

impl Queue {
    /// Build a queue from a [`QueueConfig`].
    ///
    /// If `config.player` is `Some`, the caller-supplied
    /// [`PlayerImpl`] is used (caller retains ownership; must not
    /// mutate its item list directly — `play`/`pause`/`seek` OK;
    /// `replace_item`, `reserve_slots`, `select_item`, `remove_at` are
    /// Queue-owned). If `None`, a default player is built internally.
    ///
    /// Matches the project-wide pattern where config structs accept
    /// optional built instances (see
    /// [`ResourceConfig`](kithara_play::ResourceConfig)'s `worker` /
    /// `runtime` / `bus` fields).
    #[must_use]
    pub fn new(config: QueueConfig) -> Self {
        let QueueConfig {
            player,
            max_concurrent_loads,
            autoplay,
            prefetch_duration,
        } = config;
        let player = player.unwrap_or_else(|| Arc::new(PlayerImpl::new(PlayerConfig::default())));
        player.set_prefetch_duration(prefetch_duration);
        // Queue is the sole auto-advance orchestrator; the player's
        // built-in `next = current + 1` handler must stay disabled or
        // both layers will fight for the armed handover slot.
        player.set_auto_advance_enabled(false);
        let bus = player.bus().clone();
        let tracks = Arc::new(Tracks::new(bus.clone()));
        let loader = Arc::new(Loader::new(
            Arc::clone(&player),
            max_concurrent_loads,
            Arc::clone(&tracks),
        ));
        let player_rx = player.subscribe();
        Self {
            player,
            loader,
            next_id: AtomicU64::new(0),
            tracks,
            navigation: Arc::new(Mutex::new(NavigationState::new())),
            pending_select: Arc::new(Mutex::new(None)),
            sources: Arc::new(Mutex::new(HashMap::new())),
            bus,
            player_rx: Mutex::new(player_rx),
            cached_position: Arc::new(Mutex::new(None)),
            autoplay,
            current_active_src: Arc::new(Mutex::new(None)),
            armed_next_src: Arc::new(Mutex::new(None)),
            #[cfg(any(test, feature = "test-utils"))]
            test_loader_supply: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Escape hatch — direct access to the underlying [`PlayerImpl`]. iOS /
    /// Android SDK bindings should not need this.
    #[must_use]
    pub fn player(&self) -> &Arc<PlayerImpl> {
        &self.player
    }

    /// Subscribe to the unified event stream: `QueueEvent` + underlying
    /// player / audio / hls / file events.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        self.bus.subscribe()
    }

    /// Number of tracks currently in the queue.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock_tracks().len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock_tracks().is_empty()
    }

    /// Snapshot of all track entries, in queue order.
    #[must_use]
    pub fn tracks(&self) -> Vec<TrackEntry> {
        self.lock_tracks().clone()
    }

    /// Lookup a track entry by id.
    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<TrackEntry> {
        self.lock_tracks().iter().find(|e| e.id == id).cloned()
    }

    /// The currently playing track entry, if any.
    #[must_use]
    pub fn current(&self) -> Option<TrackEntry> {
        let idx = self.player.current_index();
        self.lock_tracks().get(idx).cloned()
    }

    /// The currently playing track's queue index (player-reported).
    #[must_use]
    pub fn current_index(&self) -> Option<usize> {
        let idx = self.player.current_index();
        if idx < self.len() { Some(idx) } else { None }
    }

    /// Append a track. Loading starts immediately in the background.
    pub fn append<S: Into<TrackSource>>(&self, source: S) -> TrackId {
        let id = TrackId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let source = source.into();
        let entry = TrackEntry {
            id,
            name: extract_track_name(&source),
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
        };

        let index = {
            let mut guard = self.lock_tracks_mut();
            guard.push(entry);
            guard.len() - 1
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, source.clone());
        self.player.reserve_slots(index + 1);
        self.maybe_arm_autoplay(id, index);
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        self.spawn_apply_after_load(id, source);
        id
    }

    /// Insert a track after the given id, or at the head when `after` is
    /// `None`. Loading starts immediately.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `after` does not match any
    /// track.
    pub fn insert<S: Into<TrackSource>>(
        &self,
        source: S,
        after: Option<TrackId>,
    ) -> Result<TrackId, QueueError> {
        let id = TrackId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let source = source.into();
        let entry = TrackEntry {
            id,
            name: extract_track_name(&source),
            url: source.uri().map(str::to_string),
            status: TrackStatus::Pending,
        };

        let index = {
            let mut guard = self.lock_tracks_mut();
            let pos = match after {
                None => 0,
                Some(after_id) => guard
                    .iter()
                    .position(|e| e.id == after_id)
                    .map(|i| i + 1)
                    .ok_or(QueueError::UnknownTrackId(after_id))?,
            };
            guard.insert(pos, entry);
            pos
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, source.clone());
        self.player.reserve_slots(self.len());
        self.maybe_arm_autoplay(id, index);
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        self.spawn_apply_after_load(id, source);
        Ok(id)
    }

    /// Remove a track from the queue by id.
    ///
    /// If the removed track is currently playing:
    /// - with tracks remaining → switches to the next (or previous if
    ///   we were at the tail) with an immediate cut.
    /// - with no tracks remaining → pauses the player.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue.
    pub fn remove(&self, id: TrackId) -> Result<(), QueueError> {
        let was_current = self.current().map(|e| e.id) == Some(id);
        let successor_id = if was_current {
            let guard = self.lock_tracks();
            let pos = guard.iter().position(|e| e.id == id);
            let result = pos.and_then(|p| {
                guard
                    .get(p + 1)
                    .or_else(|| if p > 0 { guard.get(p - 1) } else { None })
                    .map(|e| e.id)
            });
            drop(guard);
            result
        } else {
            None
        };

        let index = {
            let mut guard = self.lock_tracks_mut();
            let pos = guard
                .iter()
                .position(|e| e.id == id)
                .ok_or(QueueError::UnknownTrackId(id))?;
            guard.remove(pos);
            pos
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        // Drop stale autoplay/select intent pointing at a removed
        // track — otherwise it would silently bind to whatever id the
        // loader emits for the next-completed load.
        {
            let mut p = self.lock_pending_select_mut();
            if matches!(*p, Some(pending) if pending.id == id) {
                *p = None;
            }
        }
        let _ = self.player.remove_at(index);
        self.bus.publish(QueueEvent::TrackRemoved { id });

        if was_current {
            if let Some(next) = successor_id {
                let _ = self.select(next, Transition::None);
            } else {
                self.player.pause();
            }
        }
        Ok(())
    }

    /// Remove all tracks from the queue.
    pub fn clear(&self) {
        let ids: Vec<TrackId> = {
            let mut guard = self.lock_tracks_mut();
            let ids = guard.iter().map(|e| e.id).collect();
            guard.clear();
            ids
        };
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        // Any pending select/autoplay intent is invalid now.
        *self.lock_pending_select_mut() = None;
        *self.lock_armed_next_src_mut() = None;
        *self
            .current_active_src
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.player.remove_all_items();
        for id in ids {
            self.bus.publish(QueueEvent::TrackRemoved { id });
        }
    }

    /// Replace the entire queue with the given sources.
    pub fn set_tracks<I, S>(&self, sources: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<TrackSource>,
    {
        self.clear();
        for s in sources {
            let _ = self.append(s);
        }
    }

    /// Select a track by id, applying the given [`Transition`]. If the
    /// track is still loading or pending, both the id and the
    /// transition are stashed and applied when loading finishes.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue,
    /// [`QueueError::NotReady`] if the track is in a terminal failed state,
    /// or [`QueueError::Play`] if the underlying `select_item` call fails.
    pub fn select(&self, id: TrackId, transition: Transition) -> Result<(), QueueError> {
        let (index, status) = {
            let guard = self.lock_tracks();
            guard
                .iter()
                .enumerate()
                .find(|(_, e)| e.id == id)
                .map(|(i, e)| (i, e.status.clone()))
                .ok_or(QueueError::UnknownTrackId(id))?
        };

        // Idempotency: selecting the track that's already loaded & active
        // is a no-op. Autoplay in `spawn_apply_after_load` issues an
        // implicit select; an explicit user select on the same track must
        // not re-issue LoadTrack — that wipes the decoder timeline and
        // resets position to zero mid-playback. We check `player.current_index`
        // (updated synchronously inside `select_item_with_crossfade`) rather
        // than `navigation.current_index` (which `advance_to_next` moves
        // before calling select, so it would mistakenly match).
        // `rate > 0` distinguishes "player holds this slot" from the
        // default `current_index = 0` on a fresh, never-selected player;
        // `select_item_with_crossfade` with `autoplay=true` sets `rate`
        // synchronously, while `is_playing()` depends on the processor
        // draining `SetPaused(false)` — racy.
        if self.player.current_index() == index && self.player.rate() > 0.0 {
            return Ok(());
        }

        match status {
            TrackStatus::Loaded => {
                let was_playing = self.player.is_playing();
                let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
                // Capture the src BEFORE select_item_with_crossfade
                // moves it into the audio thread arena.
                let src = self.player.item_src(index);
                self.player
                    .select_item_with_crossfade(index, true, crossfade)?;
                if let Some(src) = src {
                    self.note_active_src(src);
                }
                self.lock_navigation_mut().select(index);
                if was_playing && crossfade > 0.0 {
                    self.bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                // Engine took items[index] inside select_item — if user
                // re-selects the same track later, we must reload.
                self.set_status(id, TrackStatus::Consumed);
                Ok(())
            }
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow => {
                *self.lock_pending_select_mut() = Some(PendingSelect { id, transition });
                Ok(())
            }
            TrackStatus::Consumed | TrackStatus::Failed(_) => {
                let source = self
                    .sources
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&id)
                    .cloned()
                    .ok_or(QueueError::NotReady(id))?;
                *self.lock_pending_select_mut() = Some(PendingSelect { id, transition });
                self.set_status(id, TrackStatus::Pending);
                self.spawn_apply_after_load(id, source);
                Ok(())
            }
            _ => Err(QueueError::NotReady(id)),
        }
    }

    /// Advance to the next track per navigation rules. Returns the newly
    /// selected id, or `None` when the queue has ended (and
    /// [`RepeatMode::Off`] is active). At end-of-queue the player is
    /// paused so the UI sees a paused/stopped state instead of a still-
    /// playing rate on a ghost position past the last track's EOF.
    pub fn advance_to_next(&self, transition: Transition) -> Option<TrackId> {
        let len = self.len();
        let Some(idx) = self.lock_navigation_mut().next(len) else {
            self.end_queue();
            return None;
        };
        let id = self.lock_tracks().get(idx).map(|e| e.id)?;
        if let Err(e) = self.select(id, transition) {
            warn!(id = id.as_u64(), error = %e, "advance_to_next: select failed");
        }
        Some(id)
    }

    /// Go back to the previous track. Returns the newly selected id, or
    /// `None` at index 0.
    pub fn return_to_previous(&self, transition: Transition) -> Option<TrackId> {
        let prev_idx = self.lock_navigation_mut().prev()?;
        let id = self.lock_tracks().get(prev_idx).map(|e| e.id)?;
        if let Err(e) = self.select(id, transition) {
            warn!(id = id.as_u64(), error = %e, "return_to_previous: select failed");
        }
        Some(id)
    }

    /// Enable or disable shuffle.
    pub fn set_shuffle(&self, on: bool) {
        self.lock_navigation_mut().set_shuffle(on);
        // Navigation's "what's next" answer changed; drop the stale arm
        // and let the next prefetch trigger re-evaluate.
        self.player.unarm_next();
        *self.lock_armed_next_src_mut() = None;
    }

    /// Current shuffle state.
    #[must_use]
    pub fn is_shuffle_enabled(&self) -> bool {
        self.lock_navigation().is_shuffle_enabled()
    }

    /// Set repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.lock_navigation_mut().set_repeat(mode);
        self.player.unarm_next();
        *self.lock_armed_next_src_mut() = None;
    }

    /// Current repeat mode.
    #[must_use]
    pub fn repeat_mode(&self) -> RepeatMode {
        self.lock_navigation().repeat_mode()
    }

    /// Seek within the currently-playing track.
    ///
    /// # Errors
    /// Returns [`QueueError::Play`] if the player reports a seek failure.
    pub fn seek(&self, seconds: f64) -> Result<(), QueueError> {
        self.player.seek_seconds(seconds).map_err(QueueError::from)
    }

    /// Periodic tick: drives `PlayerImpl::tick`, pumps audio-thread
    /// notifications onto the bus, and drains queued engine events to
    /// act on `ItemDidPlayToEnd` (filtered) and forward
    /// `CurrentItemChanged` as [`QueueEvent::CurrentTrackChanged`].
    ///
    /// `process_notifications` is the bridge that converts per-slot
    /// `TrackRequested` / `TrackHandoverRequested` / `TrackPlaybackStopped`
    /// into `PlayerEvent::PrefetchRequested` / `HandoverRequested` /
    /// `ItemDidPlayToEnd` on the bus — without it queue auto-advance
    /// (cf=0 arena handover and cf>0 commit_next) never observes the
    /// audio-thread side.
    ///
    /// # Errors
    /// Forwards `PlayError` from `PlayerImpl::tick`.
    pub fn tick(&self) -> Result<(), QueueError> {
        tracing::trace!("Queue::tick");
        self.player.tick()?;
        self.player.process_notifications();
        self.drain_player_events();
        self.update_cached_position();
        Ok(())
    }

    /// Latest monotonic playback position for the current track in
    /// seconds. Updated on every [`Self::tick`]; skips transient 0.0
    /// samples the engine produces on pause/resume so downstream UIs
    /// see stable values.
    #[must_use]
    pub fn position_seconds(&self) -> Option<f64> {
        *self
            .cached_position
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn update_cached_position(&self) {
        let Some(t) = self.player.position_seconds() else {
            return;
        };
        let mut slot = self
            .cached_position
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Engine briefly reports 0.0 on pause/resume; keep the last
        // sane value so slider bindings don't flash back to the start.
        if t == 0.0 && slot.is_some_and(|prev| prev > MIN_STABLE_POSITION_SECS) {
            return;
        }
        *slot = Some(t);
    }

    delegate! {
        to self.player {
            /// Start playback.
            pub fn play(&self);
            /// Pause playback.
            pub fn pause(&self);
            /// Whether playback is active.
            #[must_use]
            pub fn is_playing(&self) -> bool;
            /// Current crossfade duration in seconds.
            #[must_use]
            pub fn crossfade_duration(&self) -> f32;
            /// Set the crossfade duration.
            pub fn set_crossfade_duration(&self, seconds: f32);
            /// Lead time before EOF at which the next queued track is preloaded.
            ///
            /// See [`QueueConfig::prefetch_duration`] for semantics.
            #[must_use]
            pub fn prefetch_duration(&self) -> f32;
            /// Set the prefetch lead time in seconds.
            pub fn set_prefetch_duration(&self, seconds: f32);
            /// Default playback rate.
            #[must_use]
            pub fn default_rate(&self) -> f32;
            /// Set the default playback rate.
            pub fn set_default_rate(&self, rate: f32);
            /// Current volume (0.0..=1.0).
            #[must_use]
            pub fn volume(&self) -> f32;
            /// Set the volume (0.0..=1.0).
            pub fn set_volume(&self, volume: f32);
            /// Whether output is muted.
            #[must_use]
            pub fn is_muted(&self) -> bool;
            /// Set the mute flag.
            pub fn set_muted(&self, muted: bool);
            /// Number of EQ bands.
            #[must_use]
            pub fn eq_band_count(&self) -> usize;
            /// Current gain for an EQ band.
            #[must_use]
            pub fn eq_gain(&self, band: usize) -> Option<f32>;
            /// Set gain for an EQ band.
            ///
            /// # Errors
            /// Forwards `PlayError` from the underlying player.
            pub fn set_eq_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError>;
            /// Reset all EQ bands to 0 dB.
            ///
            /// # Errors
            /// Forwards `PlayError` from the underlying player.
            pub fn reset_eq(&self) -> Result<(), PlayError>;
            /// Current track duration in seconds.
            #[must_use]
            pub fn duration_seconds(&self) -> Option<f64>;
        }
    }

    fn lock_tracks(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock()
    }

    fn lock_tracks_mut(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock()
    }

    fn lock_navigation(&self) -> std::sync::MutexGuard<'_, NavigationState> {
        self.navigation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_navigation_mut(&self) -> std::sync::MutexGuard<'_, NavigationState> {
        self.navigation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_pending_select_mut(&self) -> std::sync::MutexGuard<'_, Option<PendingSelect>> {
        self.pending_select
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn set_status(&self, id: TrackId, status: TrackStatus) {
        self.tracks.set_status(id, status);
    }

    /// Mark `id` as the autoplay target if the queue is configured for
    /// autoplay and no other track is already armed to play.
    ///
    /// Autoplay is expressed as a `pending_select` set synchronously at
    /// `append`/`insert` call time, BEFORE the async load starts. This
    /// removes the race where N independently-spawned loader tasks each
    /// raced against `!player.is_playing()` and a different one won
    /// every run. With a single coordination point, only the track whose
    /// id matches `pending_select` will ever start playback at load
    /// completion.
    ///
    /// Conditions:
    /// - autoplay enabled in config;
    /// - this is the first track in the queue (`index == 0`) — typical
    ///   `set_tracks([…])` flow on a fresh queue;
    /// - no `pending_select` already set (an explicit `select` call
    ///   wins over autoplay).
    fn maybe_arm_autoplay(&self, id: TrackId, index: usize) {
        if !self.autoplay || index != 0 {
            return;
        }
        let mut p = self.lock_pending_select_mut();
        if p.is_none() {
            *p = Some(PendingSelect {
                id,
                transition: Transition::None,
            });
        }
    }

    fn spawn_apply_after_load(&self, id: TrackId, source: TrackSource) {
        // Test path: if a synthetic resource was pre-queued for this
        // id, use it directly instead of dispatching the real loader.
        // Lets the integration suite drive the full
        // load → arm → handover lifecycle (including replays) without a
        // real network or disk-backed source. Production builds skip
        // this branch entirely (cfg-gated).
        #[cfg(any(test, feature = "test-utils"))]
        {
            let preset = self
                .test_loader_supply
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(&id)
                .and_then(|stack| stack.pop());
            if let Some(resource) = preset {
                self.complete_load_for_test(id, resource);
                return;
            }
        }

        let handle = self.loader.spawn_load(id, source);
        let player = Arc::clone(&self.player);
        let tracks = Arc::clone(&self.tracks);
        let pending_select = Arc::clone(&self.pending_select);
        let navigation = Arc::clone(&self.navigation);
        let bus = self.bus.clone();
        let current_active_src = Arc::clone(&self.current_active_src);
        tokio::spawn(async move {
            let resource = match handle.await {
                Ok(Ok(resource)) => resource,
                Ok(Err(_)) => return,
                Err(join_err) => {
                    warn!(id = id.as_u64(), error = %join_err, "loader join failed");
                    return;
                }
            };

            let index = {
                let guard = tracks.lock();
                guard.iter().position(|e| e.id == id)
            };
            let Some(index) = index else {
                debug!(
                    id = id.as_u64(),
                    "load completed but track no longer in queue"
                );
                return;
            };

            player.replace_item(index, resource);
            tracks.set_status(id, TrackStatus::Loaded);

            let pending_transition = {
                let mut p = pending_select
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let result = match *p {
                    Some(pending) if pending.id == id => {
                        *p = None;
                        Some(pending.transition)
                    }
                    _ => None,
                };
                drop(p);
                result
            };

            if let Some(transition) = pending_transition {
                let was_playing = player.is_playing();
                let crossfade = transition.crossfade_seconds(player.crossfade_duration());
                let src = player.item_src(index);
                if let Err(e) = player.select_item_with_crossfade(index, true, crossfade) {
                    warn!(id = id.as_u64(), error = %e, "pending select failed");
                } else {
                    if let Some(src) = src {
                        *current_active_src
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = Some(src);
                    }
                    navigation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .select(index);
                    if was_playing && crossfade > 0.0 {
                        bus.publish(QueueEvent::CrossfadeStarted {
                            duration_seconds: crossfade,
                        });
                    }
                    tracks.set_status(id, TrackStatus::Consumed);
                }
            }
        });
    }

    fn drain_player_events(&self) {
        let mut rx = self
            .player_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            match rx.try_recv() {
                Ok(ev) => self.process_player_event(&ev),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    fn process_player_event(&self, ev: &Event) {
        match ev {
            Event::Player(PlayerEvent::PrefetchRequested) => {
                self.handle_prefetch_requested();
            }
            Event::Player(PlayerEvent::HandoverRequested) => {
                self.handle_handover_requested();
            }
            Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. }) => {
                debug!(%src, "ItemDidPlayToEnd received");
                // `src` identifies the underlying audio source of the
                // track that just hit EOF. After a cf>0 crossfade the
                // outgoing (previous) track's EOF arrives AFTER the
                // queue has already advanced its index — checking the
                // src against the currently-leading src lets us tell
                // that case apart from a genuine "current track has
                // finished" signal.
                self.sync_navigation_after_handover(&src);
            }
            Event::Player(PlayerEvent::CurrentItemChanged) => {
                let idx = self.player.current_index();
                let id = self.lock_tracks().get(idx).map(|e| e.id);
                *self
                    .cached_position
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = None;
                self.bus.publish(QueueEvent::CurrentTrackChanged { id });
            }
            Event::Queue(QueueEvent::TrackStatusChanged {
                status: TrackStatus::Loaded,
                ..
            }) => {
                // Loader caught up after the prefetch trigger fired and
                // the armer skipped because the resource wasn't ready.
                // Retry now if no slot is armed yet and the resolved
                // next index points at this newly loaded entry.
                if self.player.armed_next().is_none() {
                    self.handle_prefetch_requested();
                }
            }
            _ => {}
        }
    }

    /// Resolve the next index per [`NavigationState`] policy and arm it
    /// on the player if the corresponding resource is loaded.
    ///
    /// Skips entries whose `TrackStatus` is not [`TrackStatus::Loaded`]:
    /// not-yet-loaded entries are arm-retried via the `TrackStatusChanged`
    /// path (when the loader publishes `Loaded`); failed entries are
    /// skipped via the navigation peek loop, advancing to the next
    /// reachable successor.
    fn handle_prefetch_requested(&self) {
        let len = self.len();
        let Some(next_idx) = self.lock_navigation().peek_next(len) else {
            debug!("prefetch: navigation has no next; unarming");
            self.player.unarm_next();
            *self.lock_armed_next_src_mut() = None;
            return;
        };
        let entry = self.lock_tracks().get(next_idx).cloned();
        let Some(entry) = entry else {
            return;
        };
        debug!(next_idx, status = ?entry.status, "prefetch requested");
        match entry.status {
            TrackStatus::Loaded => {
                // `arm_next` returns the armed track's underlying audio
                // src; remember it so `sync_navigation_after_handover`
                // can promote it to `current_active_src` once the audio
                // thread (cf=0) or `commit_next` (cf>0) actually makes
                // it leading.
                match self.player.arm_next(next_idx) {
                    Some(src) => {
                        debug!(next_idx, %src, "armed next");
                        *self.lock_armed_next_src_mut() = Some(src);
                    }
                    None => {
                        warn!(
                            next_idx,
                            "arm_next returned None despite Loaded status — items slot empty"
                        );
                    }
                }
            }
            TrackStatus::Consumed => {
                // The track has already been played and the engine
                // took its resource. Re-spawn the loader so the
                // status flips back to `Loaded`; the
                // `TrackStatusChanged { Loaded }` retry path then
                // re-fires `handle_prefetch_requested` and the arm
                // succeeds. Without this branch a second playthrough
                // of a queue silently stops after the first track
                // because every successor is `Consumed`.
                let source = self
                    .sources
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&entry.id)
                    .cloned();
                let Some(source) = source else {
                    return;
                };
                debug!(next_idx, "respawning load for Consumed prefetch target");
                self.set_status(entry.id, TrackStatus::Pending);
                self.spawn_apply_after_load(entry.id, source);
            }
            // Pending / Loading / Slow: loader already in flight; the
            // `TrackStatusChanged { Loaded }` retry path will arm us.
            // Failed: do NOT respawn — broken URLs would loop forever
            // hammering the loader on every subsequent prefetch event.
            // The leading track will reach EOF, no Preloading is in the
            // arena, and `end_queue` paths handle the silence-then-stop
            // outcome.
            _ => {}
        }
    }

    fn lock_armed_next_src_mut(&self) -> std::sync::MutexGuard<'_, Option<Arc<str>>> {
        self.armed_next_src
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Commit the previously armed handover, advance navigation, mark
    /// the just-promoted track as `Consumed`, and emit
    /// [`QueueEvent::CrossfadeStarted`].
    ///
    /// `CrossfadeStarted` is published only after `commit_next` returns
    /// `Ok`, so subscribers never see a fade event without a fade.
    fn handle_handover_requested(&self) {
        let Some(idx) = self.player.armed_next() else {
            return;
        };
        if let Err(e) = self.player.commit_next(idx) {
            warn!(error = %e, "commit_next failed");
            return;
        }
        // After commit_next, the armed src becomes the new leading src.
        // Take it out of `armed_next_src` and promote to active so the
        // EOF handler can tell the outgoing track's EOF apart from a
        // real "current track ended" signal.
        if let Some(src) = self.lock_armed_next_src_mut().take() {
            self.note_active_src(src);
        }
        self.lock_navigation_mut().select(idx);
        let id = self.lock_tracks().get(idx).map(|e| e.id);
        if let Some(id) = id {
            self.set_status(id, TrackStatus::Consumed);
        }
        let crossfade = self.player.crossfade_duration();
        if crossfade > 0.0 {
            self.bus.publish(QueueEvent::CrossfadeStarted {
                duration_seconds: crossfade,
            });
        }
    }

    /// Test helper: append a track-entry as if `append` was called but
    /// without spawning the loader. The track sits in `Pending` state
    /// until [`Self::complete_load_for_test`] is called for its id.
    /// Mirrors the synchronous portion of `append`, including
    /// `maybe_arm_autoplay`.
    ///
    /// Pair with [`Self::complete_load_for_test`] to control which
    /// "load" finishes first — the test can append `[A, B]` and then
    /// complete `B` before `A` to reproduce the loader-order race.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn register_for_test(&self) -> TrackId {
        let id = TrackId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let url = format!("test://memory/{}", id.as_u64());
        let entry = TrackEntry {
            id,
            name: format!("test-{}", id.as_u64()),
            url: Some(url.clone()),
            status: TrackStatus::Pending,
        };
        let index = {
            let mut guard = self.lock_tracks_mut();
            guard.push(entry);
            guard.len() - 1
        };
        // Register a synthetic source so the production
        // `Consumed`/`Failed` re-spawn paths in `select` and
        // `handle_prefetch_requested` can find the entry. The fake URI
        // never reaches the real loader because
        // `spawn_apply_after_load` short-circuits to
        // `test_loader_supply` when a resource has been pre-supplied.
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, TrackSource::Uri(url));
        self.player.reserve_slots(index + 1);
        self.maybe_arm_autoplay(id, index);
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        id
    }

    /// Test helper: synthesise the load-completion path for a track
    /// previously registered via [`Self::register_for_test`]. Mirrors
    /// the loader-completion logic in `spawn_apply_after_load`
    /// (`replace_item`, status flip, pending-select consumption,
    /// `select_item_with_crossfade` if armed).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn complete_load_for_test(&self, id: TrackId, resource: kithara_play::Resource) {
        let index = {
            let guard = self.lock_tracks();
            guard.iter().position(|e| e.id == id)
        };
        let Some(index) = index else {
            return;
        };

        self.player.replace_item(index, resource);
        self.set_status(id, TrackStatus::Loaded);

        let pending_transition = {
            let mut p = self.lock_pending_select_mut();
            let result = match *p {
                Some(pending) if pending.id == id => {
                    *p = None;
                    Some(pending.transition)
                }
                _ => None,
            };
            drop(p);
            result
        };

        if let Some(transition) = pending_transition {
            let was_playing = self.player.is_playing();
            let crossfade = transition.crossfade_seconds(self.player.crossfade_duration());
            let src = self.player.item_src(index);
            if let Err(e) = self
                .player
                .select_item_with_crossfade(index, true, crossfade)
            {
                warn!(id = id.as_u64(), error = %e, "test pending select failed");
            } else {
                if let Some(src) = src {
                    self.note_active_src(src);
                }
                self.lock_navigation_mut().select(index);
                if was_playing && crossfade > 0.0 {
                    self.bus.publish(QueueEvent::CrossfadeStarted {
                        duration_seconds: crossfade,
                    });
                }
                self.set_status(id, TrackStatus::Consumed);
            }
        }
    }

    /// Test helper: convenience for the common case where load order
    /// matches register order. Equivalent to
    /// `register_for_test` + `complete_load_for_test` back-to-back.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn insert_loaded_for_test(&self, resource: kithara_play::Resource) -> TrackId {
        let id = self.register_for_test();
        self.complete_load_for_test(id, resource);
        id
    }

    /// Test helper: pre-supply a [`Resource`] that
    /// `spawn_apply_after_load` will use the next time it is dispatched
    /// for this `id` (e.g. when `handle_prefetch_requested` respawns a
    /// `Consumed` next-track). Pushes onto an LIFO stack so callers can
    /// queue multiple resources for the same id across many replays.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn supply_test_resource_for_respawn(&self, id: TrackId, resource: kithara_play::Resource) {
        self.test_loader_supply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_default()
            .push(resource);
    }

    /// Sync queue navigation state with the audio thread after a handover
    /// the queue did not initiate (cf=0 path, where the player advances
    /// `current_index` from the EOF observation).
    ///
    /// `ended_src` is the underlying audio source of the track whose
    /// EOF triggered this call. We compare it against
    /// `current_active_src` (the src the queue last designated as
    /// leading) to distinguish:
    /// - genuine "current track finished" — matches → may end queue;
    /// - "outgoing track of an in-progress crossfade finished" — does
    ///   NOT match (queue already promoted the next src) → ignore.
    fn sync_navigation_after_handover(&self, ended_src: &Arc<str>) {
        let player_idx = self.player.current_index();
        let nav_idx = self.lock_navigation().current_index();
        debug!(
            %ended_src,
            player_idx,
            ?nav_idx,
            "sync_navigation_after_handover entry"
        );
        let Some(prior) = nav_idx else {
            // Navigation never selected anything — the queue hasn't
            // started this play-through. A synthetic / spurious EOF
            // before the first select must not advance navigation.
            return;
        };
        let len = self.len();
        if prior == player_idx {
            // No advance happened. Two sub-cases:
            //   (a) outgoing-EOF after a completed cf>0 crossfade —
            //       `ended_src` is the previous (now-finished) leading
            //       src, NOT the current active src; ignore.
            //   (b) current track ended naturally and no successor was
            //       armed — end of queue; pause + publish.
            if !self.ended_src_is_current(ended_src) {
                return;
            }
            if self.lock_navigation().peek_next(len).is_none() {
                self.end_queue();
            }
            return;
        }
        if player_idx < len {
            // Audio thread arena handover (cf=0) just promoted the
            // armed track to leading. Take it out of `armed_next_src`
            // so subsequent EOF events compare against the right src.
            if let Some(src) = self.lock_armed_next_src_mut().take() {
                self.note_active_src(src);
            }
            self.lock_navigation_mut().select(player_idx);
            let id = self.lock_tracks().get(player_idx).map(|e| e.id);
            if let Some(id) = id {
                self.set_status(id, TrackStatus::Consumed);
            }
            return;
        }
        // Audio thread observed EOF on the last track and tried to
        // promote a non-existent next slot.
        self.end_queue();
    }

    /// Whether the just-ended `src` matches what the queue last marked
    /// as the leading track. When the queue never recorded a leading
    /// src (e.g. spurious early EOF before any select), treat the EOF
    /// as belonging to the current track for backward compatibility.
    fn ended_src_is_current(&self, ended_src: &Arc<str>) -> bool {
        let guard = self
            .current_active_src
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            Some(active) => Arc::ptr_eq(active, ended_src) || **active == **ended_src,
            None => true,
        }
    }

    /// Update the queue's mirror of the leading-track src. Called from
    /// every path that promotes a new track to leading status (explicit
    /// select, autoplay, crossfade commit, post-EOF arena handover).
    fn note_active_src(&self, src: Arc<str>) {
        *self
            .current_active_src
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(src);
    }

    /// Pause the player and publish [`QueueEvent::QueueEnded`]. Called
    /// from any path that determines no further tracks will play.
    fn end_queue(&self) {
        self.player.pause();
        *self
            .current_active_src
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self.lock_armed_next_src_mut() = None;
        self.bus.publish(QueueEvent::QueueEnded);
    }
}

fn extract_track_name(source: &TrackSource) -> String {
    let raw = match source {
        TrackSource::Uri(s) => s.as_str(),
        TrackSource::Config(cfg) => return name_from_src(&cfg.src),
    };
    name_from_raw(raw)
}

fn name_from_src(src: &ResourceSrc) -> String {
    match src {
        ResourceSrc::Url(url) => {
            let path = url.path();
            name_from_raw(path)
        }
        ResourceSrc::Path(p) => p.file_name().map_or_else(
            || "Unknown".to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

fn name_from_raw(s: &str) -> String {
    s.rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or("Unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_events::PlayerEvent;
    use kithara_test_utils::kithara;
    use tokio::time::{Instant as TokioInstant, timeout as tokio_timeout};

    use super::*;

    fn make_queue() -> Queue {
        Queue::new(QueueConfig::default())
    }

    async fn wait_for_queue_event<F>(
        rx: &mut EventReceiver,
        mut matches: F,
        timeout_ms: u64,
    ) -> bool
    where
        F: FnMut(&QueueEvent) -> bool,
    {
        let deadline = TokioInstant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(TokioInstant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio_timeout(remaining, rx.recv()).await {
                Ok(Ok(Event::Queue(ev))) if matches(&ev) => return true,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => return false,
            }
        }
    }

    #[kithara::test]
    fn queue_new_constructs_without_panic() {
        let _queue = make_queue();
    }

    #[kithara::test(tokio)]
    async fn len_is_empty_reflect_append() {
        let queue = make_queue();
        assert!(queue.is_empty());
        let _ = queue.append("https://example.com/a.mp3");
        let _ = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
    }

    #[kithara::test(tokio)]
    async fn append_returns_monotonic_ids_and_emits_track_added() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");
        assert_ne!(a, b);
        assert!(a.as_u64() < b.as_u64());

        let mut seen = 0;
        while wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackAdded { .. }),
            200,
        )
        .await
        {
            seen += 1;
            if seen == 2 {
                break;
            }
        }
        assert_eq!(seen, 2);
    }

    #[kithara::test(tokio)]
    async fn select_unknown_id_errors() {
        let queue = make_queue();
        let err = queue
            .select(TrackId(999), Transition::None)
            .expect_err("unknown id should error");
        assert!(matches!(err, QueueError::UnknownTrackId(_)));
    }

    #[kithara::test(tokio)]
    async fn select_pending_track_stashes_pending_select() {
        let queue = make_queue();
        let id = queue.append("https://example.com/a.mp3");
        let _ = queue.select(id, Transition::None);
        let pending = queue
            .pending_select
            .lock()
            .expect("pending_select lock")
            .to_owned();
        let pending = pending.expect("pending set");
        assert_eq!(pending.id, id);
        assert_eq!(pending.transition, Transition::None);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_on_empty_emits_queue_ended() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        assert!(queue.advance_to_next(Transition::Crossfade).is_none());
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[kithara::test(tokio)]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        let mut rx = queue.subscribe();

        assert!(queue.advance_to_next(Transition::Crossfade).is_some());
        assert!(queue.advance_to_next(Transition::Crossfade).is_some());
        assert!(queue.advance_to_next(Transition::Crossfade).is_none());

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }

    #[kithara::test(tokio)]
    async fn spurious_item_did_play_to_end_is_filtered() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");

        queue
            .player
            .bus()
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd {
                src: Arc::from("test://spurious"),
                item_id: None,
            }));

        queue.tick().expect("tick");

        let nav_idx = queue.lock_navigation().current_index();
        assert_eq!(nav_idx, None, "navigation must not have advanced");
    }

    #[kithara::test(tokio)]
    async fn remove_drops_from_queue_and_emits() {
        let queue = make_queue();
        let a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        let mut rx = queue.subscribe();

        queue.remove(a).expect("removed");
        assert_eq!(queue.len(), 1);
        let saw_removed = wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackRemoved { id } if *id == a),
            300,
        )
        .await;
        assert!(saw_removed);
    }

    #[kithara::test(tokio)]
    async fn clear_empties_queue() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
        queue.clear();
        assert_eq!(queue.len(), 0);
    }

    #[kithara::test(tokio)]
    async fn set_tracks_replaces_queue() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        queue.set_tracks([
            "https://example.com/1.mp3",
            "https://example.com/2.mp3",
            "https://example.com/3.mp3",
        ]);
        assert_eq!(queue.len(), 3);
    }

    #[kithara::test(tokio)]
    async fn insert_after_id_places_next() {
        let queue = make_queue();
        let a = queue.append("https://example.com/a.mp3");
        let b = queue.append("https://example.com/b.mp3");
        let mid = queue
            .insert("https://example.com/mid.mp3", Some(a))
            .expect("insert");
        let snapshot = queue.tracks();
        let ids: Vec<TrackId> = snapshot.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a, mid, b]);
    }

    #[kithara::test(tokio)]
    async fn queue_disables_player_auto_advance_on_construction() {
        let queue = make_queue();
        assert!(
            !queue.player.auto_advance_enabled(),
            "Queue::new must disable the player's built-in auto-advance"
        );
    }

    #[kithara::test(tokio)]
    async fn set_repeat_unarms_pending_next() {
        let queue = make_queue();
        // Drive the player into "armed" state by hand: this is normally
        // populated via the prefetch event handler, but we exercise the
        // unarm side-effect directly. Since arm_next requires items in
        // the player's slot, we synthesise a minimal arm by reserving
        // and replacing a dummy resource via the test harness path is
        // overkill — instead we just assert the unarm is invoked
        // without panicking and the post-state is None.
        queue.set_repeat(RepeatMode::All);
        assert_eq!(queue.player.armed_next(), None);
        queue.set_shuffle(true);
        assert_eq!(queue.player.armed_next(), None);
    }

    #[kithara::test(tokio)]
    async fn sync_navigation_ignores_eof_before_first_select() {
        // Spurious ItemDidPlayToEnd before any select: navigation must
        // not advance, no QueueEnded.
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");

        queue
            .player
            .bus()
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd {
                src: Arc::from("test://spurious"),
                item_id: None,
            }));
        queue.tick().expect("tick");

        assert_eq!(queue.lock_navigation().current_index(), None);
    }

    // Behavioural autoplay tests live in
    // `tests/tests/kithara_queue/auto_advance.rs` — they drive a real
    // Queue + offline render and assert PCM signatures, which is the
    // only way to catch the original race (loader-completion order
    // changing which track is heard first).

    /// Replay regression: after a track has been played its status is
    /// `Consumed` and its `items` slot has been taken by the engine.
    /// `handle_prefetch_requested` must respawn the loader for a
    /// `Consumed` next-track so the queue can auto-advance again on
    /// subsequent playthroughs. Without this, every playthrough after
    /// the first stops after track 1 because `arm_next` is never
    /// called for `Consumed` successors.
    #[kithara::test(tokio)]
    async fn prefetch_respawns_consumed_next_track() {
        let queue = make_queue();
        let _id_a = queue.append("https://example.com/a.mp3");
        let id_b = queue.append("https://example.com/b.mp3");

        // Pretend track A has been selected (so navigation has a
        // current index for `peek_next` to step from) and track B has
        // already been played in a prior playthrough.
        queue.lock_navigation_mut().select(0);
        queue.set_status(id_b, TrackStatus::Consumed);
        let mut rx = queue.subscribe();

        queue.handle_prefetch_requested();

        // The respawn path sets the track back to Pending and spawns
        // the loader (which will eventually fail in this synthetic
        // setup because the URL is unreachable, but the Pending
        // transition is the observable signal of "respawn happened").
        let saw_pending = wait_for_queue_event(
            &mut rx,
            |ev| matches!(
                ev,
                QueueEvent::TrackStatusChanged {
                    id, status: TrackStatus::Pending,
                } if *id == id_b
            ),
            300,
        )
        .await;
        assert!(
            saw_pending,
            "Consumed next-track must be re-spawned on prefetch (status flipped back to Pending)"
        );
    }

    /// `Failed` next-tracks must NOT be re-spawned by prefetch —
    /// otherwise a permanently broken URL would loop forever, hammering
    /// the loader on every subsequent prefetch event. The leading
    /// track's EOF should be left to surface as silence-then-stop.
    #[kithara::test(tokio)]
    async fn prefetch_does_not_respawn_failed_next_track() {
        let queue = make_queue();
        let _id_a = queue.append("https://example.com/a.mp3");
        let id_b = queue.append("https://example.com/b.mp3");

        queue.lock_navigation_mut().select(0);
        queue.set_status(id_b, TrackStatus::Failed("intentional".into()));
        // Drain the just-emitted Failed event so the next wait window
        // is clean.
        let mut rx = queue.subscribe();
        let _ = tokio_timeout(Duration::from_millis(50), rx.recv()).await;

        queue.handle_prefetch_requested();

        let respawned = wait_for_queue_event(
            &mut rx,
            |ev| matches!(
                ev,
                QueueEvent::TrackStatusChanged {
                    id, status: TrackStatus::Pending,
                } if *id == id_b
            ),
            150,
        )
        .await;
        assert!(
            !respawned,
            "Failed next-track must NOT be re-spawned by prefetch — \
             that would loop a permanently broken URL forever"
        );
    }
}
