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
    /// Tracks the id of the track whose crossfade-advance has already
    /// been armed during `tick()`. Prevents triggering the next-track
    /// select repeatedly as the remaining playtime keeps ticking below
    /// the crossfade threshold. Cleared on [`QueueEvent::CurrentTrackChanged`].
    crossfade_armed_for: Arc<Mutex<Option<TrackId>>>,
    /// Authoritative playback position updated on every [`Self::tick`].
    /// Filters transient 0.0 blips the engine reports on pause/resume —
    /// downstream UIs should read from this field rather than polling
    /// the engine directly.
    cached_position: Arc<Mutex<Option<f64>>>,
    autoplay: bool,
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
        } = config;
        let player = player.unwrap_or_else(|| Arc::new(PlayerImpl::new(PlayerConfig::default())));
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
            crossfade_armed_for: Arc::new(Mutex::new(None)),
            cached_position: Arc::new(Mutex::new(None)),
            autoplay,
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
                self.player
                    .select_item_with_crossfade(index, true, crossfade)?;
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
    /// [`RepeatMode::Off`] is active).
    pub fn advance_to_next(&self, transition: Transition) -> Option<TrackId> {
        let len = self.len();
        let Some(idx) = self.lock_navigation_mut().next(len) else {
            self.bus.publish(QueueEvent::QueueEnded);
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
    }

    /// Current shuffle state.
    #[must_use]
    pub fn is_shuffle_enabled(&self) -> bool {
        self.lock_navigation().is_shuffle_enabled()
    }

    /// Set repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.lock_navigation_mut().set_repeat(mode);
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

    /// Periodic tick: drives `PlayerImpl::tick` and drains queued engine
    /// events to act on `ItemDidPlayToEnd` (filtered) and forward
    /// `CurrentItemChanged` as [`QueueEvent::CurrentTrackChanged`].
    ///
    /// # Errors
    /// Forwards `PlayError` from `PlayerImpl::tick`.
    pub fn tick(&self) -> Result<(), QueueError> {
        self.player.tick()?;
        self.drain_player_events();
        self.update_cached_position();
        self.maybe_arm_crossfade();
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

    /// Start the next-track crossfade ahead of end-of-track when the
    /// remaining playtime drops below the configured crossfade window,
    /// so the two tracks actually overlap. `ItemDidPlayToEnd` alone
    /// fires after the first track is already silent — too late for a
    /// real crossfade.
    fn maybe_arm_crossfade(&self) {
        let crossfade = self.player.crossfade_duration();
        let Some(dur) = self.player.duration_seconds() else {
            return;
        };
        let Some(pos) = self.position_seconds() else {
            return;
        };
        let Some(entry) = self.current() else { return };
        let armed_for = *self.lock_crossfade_armed_mut();
        if !should_arm_crossfade(pos, dur, crossfade, entry.id, armed_for) {
            return;
        }
        *self.lock_crossfade_armed_mut() = Some(entry.id);
        let transition = if crossfade > 0.0 {
            Transition::Crossfade
        } else {
            Transition::None
        };
        let _ = self.advance_to_next(transition);
    }

    fn lock_crossfade_armed_mut(&self) -> std::sync::MutexGuard<'_, Option<TrackId>> {
        self.crossfade_armed_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

    fn spawn_apply_after_load(&self, id: TrackId, source: TrackSource) {
        let handle = self.loader.spawn_load(id, source);
        let player = Arc::clone(&self.player);
        let tracks = Arc::clone(&self.tracks);
        let pending_select = Arc::clone(&self.pending_select);
        let navigation = Arc::clone(&self.navigation);
        let bus = self.bus.clone();
        let autoplay = self.autoplay;
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
            let mark_consumed = || {
                tracks.set_status(id, TrackStatus::Consumed);
            };

            if let Some(transition) = pending_transition {
                let was_playing = player.is_playing();
                let crossfade = transition.crossfade_seconds(player.crossfade_duration());
                if let Err(e) = player.select_item_with_crossfade(index, true, crossfade) {
                    warn!(id = id.as_u64(), error = %e, "pending select failed");
                } else {
                    navigation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .select(index);
                    if was_playing && crossfade > 0.0 {
                        bus.publish(QueueEvent::CrossfadeStarted {
                            duration_seconds: crossfade,
                        });
                    }
                    mark_consumed();
                }
            } else if autoplay && !player.is_playing() {
                // First autoplay is always an immediate cut — nothing
                // is playing yet, so there's nothing to fade from.
                if let Err(e) = player.select_item_with_crossfade(index, true, 0.0) {
                    warn!(id = id.as_u64(), error = %e, "autoplay select failed");
                } else {
                    navigation
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .select(index);
                    mark_consumed();
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
        /// Threshold for filtering spurious `PlayerEvent::ItemDidPlayToEnd`
        /// events emitted by crossfade fade-outs of non-current tracks.
        const ITEM_END_POSITION_TOLERANCE_SECONDS: f64 = 1.0;

        match ev {
            Event::Player(PlayerEvent::ItemDidPlayToEnd) => {
                let pos = self.player.position_seconds().unwrap_or(0.0);
                let dur = self.player.duration_seconds().unwrap_or(0.0);
                // Crossfade fade-outs emit ItemDidPlayToEnd for the
                // previous track right after the swap, so the engine
                // reports `pos` near 0 — those are the spurious
                // deliveries we filter. Any ItemDidPlayToEnd past the
                // just-switched window is a real end (natural or from
                // a fatal decode error after a failed seek) and must
                // advance the queue, otherwise playback hangs.
                // If we already armed the advance from tick() while the
                // outgoing track was still playing, the engine's
                // subsequent ItemDidPlayToEnd is the trailing signal
                // for the same track — consume it without advancing
                // again.
                let armed = self.lock_crossfade_armed_mut().take();
                if armed.is_some() {
                    debug!(pos, dur, "consumed ItemDidPlayToEnd (armed pre-end)");
                    return;
                }
                // Real end-of-track: position has reached duration
                // within tolerance. Anything else is a stale / fake
                // signal (e.g. decoder-failure pos stamp, crossfade
                // fade-out on previous track).
                if dur > 0.0 && pos >= dur - ITEM_END_POSITION_TOLERANCE_SECONDS {
                    let _ = self.advance_to_next(Transition::Crossfade);
                } else {
                    debug!(pos, dur, "filtered spurious ItemDidPlayToEnd");
                }
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
            _ => {}
        }
    }
}

/// Decide whether [`Queue::tick`] should arm the pre-end advance.
///
/// Returns `true` when:
/// - `pos` and `dur` are positive (track has meaningful position + duration), AND
/// - remaining playtime is below the arm threshold — either
///   `crossfade` seconds (with crossfade > 0) or [`END_PROXIMITY_SECONDS`]
///   (no crossfade, trigger right at the tail), AND
/// - we haven't already armed for this track this play-through.
pub(crate) fn should_arm_crossfade(
    pos: f64,
    dur: f64,
    crossfade: f32,
    current_id: TrackId,
    armed_for: Option<TrackId>,
) -> bool {
    /// How close to the end we arm the next-track advance when there is
    /// no crossfade configured — gives the queue a brief window to select
    /// and start the next track before the current one goes silent.
    const END_PROXIMITY_SECONDS: f64 = 0.25;

    if dur <= 0.0 || pos <= 0.0 {
        return false;
    }
    let threshold = if crossfade > 0.0 {
        f64::from(crossfade)
    } else {
        END_PROXIMITY_SECONDS
    };
    if dur - pos > threshold {
        return false;
    }
    armed_for != Some(current_id)
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
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd));

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

    #[kithara::test]
    #[case::remaining_equals_crossfade(157.0, 162.0, 5.0, TrackId(1), None, true)]
    #[case::remaining_below_crossfade(160.0, 162.0, 5.0, TrackId(1), None, true)]
    #[case::far_from_end(100.0, 162.0, 5.0, TrackId(1), None, false)]
    #[case::already_armed_for_same_track(160.0, 162.0, 5.0, TrackId(1), Some(TrackId(1)), false)]
    #[case::armed_for_different_track_still_arms(
        160.0,
        162.0,
        5.0,
        TrackId(1),
        Some(TrackId(0)),
        true
    )]
    #[case::crossfade_zero_triggers_at_tail(161.9, 162.0, 0.0, TrackId(1), None, true)]
    #[case::crossfade_zero_quiet_middle(161.0, 162.0, 0.0, TrackId(1), None, false)]
    #[case::zero_position_rejected(0.0, 162.0, 5.0, TrackId(1), None, false)]
    #[case::zero_duration_rejected(10.0, 0.0, 5.0, TrackId(1), None, false)]
    fn should_arm_crossfade_cases(
        #[case] pos: f64,
        #[case] dur: f64,
        #[case] crossfade: f32,
        #[case] current_id: TrackId,
        #[case] armed_for: Option<TrackId>,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_arm_crossfade(pos, dur, crossfade, current_id, armed_for),
            expected
        );
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
}
