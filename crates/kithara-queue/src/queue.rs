use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

use delegate::delegate;
use kithara_events::{
    Event, EventBus, EventReceiver, PlayerEvent, QueueEvent, TrackId, TrackStatus,
};
use kithara_play::{PlayError, PlayerConfig, PlayerImpl};
use tokio::sync::broadcast::error::TryRecvError;
use tracing::{debug, warn};

use crate::{
    config::QueueConfig,
    error::QueueError,
    loader::Loader,
    navigation::{NavigationState, RepeatMode},
    track::{TrackEntry, TrackSource},
};

/// Threshold for filtering spurious `PlayerEvent::ItemDidPlayToEnd`
/// events emitted by crossfade fade-outs of non-current tracks.
const ITEM_END_POSITION_TOLERANCE_SECONDS: f64 = 1.0;

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
    tracks: Arc<Mutex<Vec<TrackEntry>>>,
    navigation: Arc<Mutex<NavigationState>>,
    pending_select: Arc<Mutex<Option<TrackId>>>,
    bus: EventBus,
    /// Subscription to the shared bus; drained in `tick()` to convert
    /// engine events into queue-level side-effects (auto-advance / current
    /// track change forwarding).
    player_rx: Mutex<EventReceiver>,
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
            net,
            store,
            max_concurrent_loads,
            autoplay,
        } = config;
        let player = player.unwrap_or_else(|| Arc::new(PlayerImpl::new(PlayerConfig::default())));
        let bus = player.bus().clone();
        let loader = Arc::new(Loader::new(
            Arc::clone(&player),
            net,
            store,
            max_concurrent_loads,
            bus.clone(),
        ));
        let player_rx = player.subscribe();
        Self {
            player,
            loader,
            next_id: AtomicU64::new(0),
            tracks: Arc::new(Mutex::new(Vec::new())),
            navigation: Arc::new(Mutex::new(NavigationState::new())),
            pending_select: Arc::new(Mutex::new(None)),
            bus,
            player_rx: Mutex::new(player_rx),
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
        self.player.reserve_slots(self.len());
        self.bus.publish(QueueEvent::TrackAdded { id, index });
        self.spawn_apply_after_load(id, source);
        Ok(id)
    }

    /// Remove a track from the queue by id.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue.
    pub fn remove(&self, id: TrackId) -> Result<(), QueueError> {
        let index = {
            let mut guard = self.lock_tracks_mut();
            let pos = guard
                .iter()
                .position(|e| e.id == id)
                .ok_or(QueueError::UnknownTrackId(id))?;
            guard.remove(pos);
            pos
        };
        let _ = self.player.remove_at(index);
        self.bus.publish(QueueEvent::TrackRemoved { id });
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

    /// Select a track by id. If the track is still loading or pending, the
    /// selection is stashed and applied when loading finishes.
    ///
    /// # Errors
    /// Returns [`QueueError::UnknownTrackId`] if `id` is not in the queue,
    /// [`QueueError::NotReady`] if the track is in a terminal failed state,
    /// or [`QueueError::Play`] if the underlying `select_item` call fails.
    pub fn select(&self, id: TrackId) -> Result<(), QueueError> {
        let (index, status, url) = {
            let guard = self.lock_tracks();
            guard
                .iter()
                .enumerate()
                .find(|(_, e)| e.id == id)
                .map(|(i, e)| (i, e.status.clone(), e.url.clone()))
                .ok_or(QueueError::UnknownTrackId(id))?
        };

        match status {
            TrackStatus::Loaded => {
                let was_playing = self.player.is_playing();
                let crossfade = self.player.crossfade_duration();
                self.player.select_item(index, true)?;
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
                *self.lock_pending_select_mut() = Some(id);
                Ok(())
            }
            TrackStatus::Consumed => {
                let Some(url) = url else {
                    return Err(QueueError::NotReady(id));
                };
                *self.lock_pending_select_mut() = Some(id);
                self.set_status(id, TrackStatus::Pending);
                self.spawn_apply_after_load(id, TrackSource::Uri(url));
                Ok(())
            }
            _ => Err(QueueError::NotReady(id)),
        }
    }

    /// Advance to the next track per navigation rules. Returns the newly
    /// selected id, or `None` when the queue has ended (and
    /// [`RepeatMode::Off`] is active).
    pub fn advance_to_next(&self) -> Option<TrackId> {
        let len = self.len();
        let Some(idx) = self.lock_navigation_mut().next(len) else {
            self.bus.publish(QueueEvent::QueueEnded);
            return None;
        };
        let id = self.lock_tracks().get(idx).map(|e| e.id)?;
        if let Err(e) = self.select(id) {
            warn!(id = id.as_u64(), error = %e, "advance_to_next: select failed");
        }
        Some(id)
    }

    /// Go back to the previous track. Returns the newly selected id, or
    /// `None` at index 0.
    pub fn return_to_previous(&self) -> Option<TrackId> {
        let prev_idx = self.lock_navigation_mut().prev()?;
        let id = self.lock_tracks().get(prev_idx).map(|e| e.id)?;
        if let Err(e) = self.select(id) {
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
        Ok(())
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
            /// Current playback position in seconds.
            #[must_use]
            pub fn position_seconds(&self) -> Option<f64>;
            /// Current track duration in seconds.
            #[must_use]
            pub fn duration_seconds(&self) -> Option<f64>;
        }
    }

    fn lock_tracks(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_tracks_mut(&self) -> std::sync::MutexGuard<'_, Vec<TrackEntry>> {
        self.tracks.lock().unwrap_or_else(PoisonError::into_inner)
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

    fn lock_pending_select_mut(&self) -> std::sync::MutexGuard<'_, Option<TrackId>> {
        self.pending_select
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn set_status(&self, id: TrackId, status: TrackStatus) {
        let mut guard = self.lock_tracks_mut();
        if let Some(entry) = guard.iter_mut().find(|e| e.id == id) {
            entry.status = status.clone();
            drop(guard);
            self.bus
                .publish(QueueEvent::TrackStatusChanged { id, status });
        }
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
                let guard = tracks.lock().unwrap_or_else(PoisonError::into_inner);
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
            {
                let mut guard = tracks.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(entry) = guard.get_mut(index) {
                    entry.status = TrackStatus::Loaded;
                }
            }
            bus.publish(QueueEvent::TrackStatusChanged {
                id,
                status: TrackStatus::Loaded,
            });

            let apply_pending = {
                let mut p = pending_select
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if *p == Some(id) {
                    *p = None;
                    true
                } else {
                    false
                }
            };
            let mark_consumed = || {
                {
                    let mut guard = tracks.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(entry) = guard.get_mut(index) {
                        entry.status = TrackStatus::Consumed;
                    }
                }
                bus.publish(QueueEvent::TrackStatusChanged {
                    id,
                    status: TrackStatus::Consumed,
                });
            };

            if apply_pending {
                let was_playing = player.is_playing();
                let crossfade = player.crossfade_duration();
                if let Err(e) = player.select_item(index, true) {
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
                if let Err(e) = player.select_item(index, true) {
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
        match ev {
            Event::Player(PlayerEvent::ItemDidPlayToEnd) => {
                let pos = self.player.position_seconds().unwrap_or(0.0);
                let dur = self.player.duration_seconds().unwrap_or(0.0);
                if dur > 0.0 && pos >= dur - ITEM_END_POSITION_TOLERANCE_SECONDS {
                    let _ = self.advance_to_next();
                } else {
                    debug!(pos, dur, "filtered spurious ItemDidPlayToEnd");
                }
            }
            Event::Player(PlayerEvent::CurrentItemChanged) => {
                let idx = self.player.current_index();
                let id = self.lock_tracks().get(idx).map(|e| e.id);
                self.bus.publish(QueueEvent::CurrentTrackChanged { id });
            }
            _ => {}
        }
    }
}

fn extract_track_name(source: &TrackSource) -> String {
    source
        .uri()
        .and_then(|s| s.rsplit('/').next())
        .unwrap_or("Unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_events::PlayerEvent;

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
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(Event::Queue(ev))) if matches(&ev) => return true,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => return false,
            }
        }
    }

    #[test]
    fn queue_new_constructs_without_panic() {
        let _queue = make_queue();
    }

    #[tokio::test]
    async fn len_is_empty_reflect_append() {
        let queue = make_queue();
        assert!(queue.is_empty());
        let _ = queue.append("https://example.com/a.mp3");
        let _ = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn select_unknown_id_errors() {
        let queue = make_queue();
        let err = queue
            .select(TrackId(999))
            .expect_err("unknown id should error");
        assert!(matches!(err, QueueError::UnknownTrackId(_)));
    }

    #[tokio::test]
    async fn select_pending_track_stashes_pending_select() {
        let queue = make_queue();
        let id = queue.append("https://example.com/a.mp3");
        let _ = queue.select(id);
        let pending = queue
            .pending_select
            .lock()
            .expect("pending_select lock")
            .to_owned();
        assert_eq!(pending, Some(id));
    }

    #[tokio::test]
    async fn advance_to_next_on_empty_emits_queue_ended() {
        let queue = make_queue();
        let mut rx = queue.subscribe();
        assert!(queue.advance_to_next().is_none());
        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 200).await;
        assert!(saw_ended);
    }

    #[tokio::test]
    async fn advance_to_next_cycles_then_emits_queue_ended() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        let mut rx = queue.subscribe();

        assert!(queue.advance_to_next().is_some());
        assert!(queue.advance_to_next().is_some());
        assert!(queue.advance_to_next().is_none());

        let saw_ended =
            wait_for_queue_event(&mut rx, |ev| matches!(ev, QueueEvent::QueueEnded), 400).await;
        assert!(saw_ended, "QueueEnded should be broadcast at end-of-queue");
    }

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
    async fn clear_empties_queue() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");
        assert_eq!(queue.len(), 2);
        queue.clear();
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
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

    #[tokio::test]
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
