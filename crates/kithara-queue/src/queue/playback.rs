use std::sync::PoisonError;

use kithara_events::{Envelope, Event, PlayerEvent, QueueEvent, TrackId, TrackStatus};
use kithara_platform::{sync::Arc, tokio::sync::broadcast::error::TryRecvError};
use tracing::debug;

use super::{
    Queue,
    types::{CachedPosition, CrossfadeArm, PlaybackView, Transition},
};
use crate::{error::QueueError, track::TrackSource};

impl Queue {
    fn advance_loaded_successor(&self, current_id: TrackId, transition: Transition) {
        let Some(next) = self.next_selectable_entry() else {
            return;
        };
        if !matches!(next.status, TrackStatus::Loaded) {
            return;
        }

        let before_index = self.player.current_index();
        if self.select(next.id, transition).is_err() {
            return;
        }
        if self.player.current_index() != before_index {
            self.write_armed_for(CrossfadeArm::armed(current_id));
        }
    }

    /// If an advance was already armed from `tick()`, consume it and
    /// return `true` — the engine's trailing `ItemDidPlayToEnd` for
    /// the same track must not advance again.
    fn consume_armed_advance(&self, ended_id: Option<TrackId>, pos: f64, dur: f64) -> bool {
        let Some(ended_id) = ended_id else {
            return false;
        };
        if self.take_armed_for_if_matches(ended_id) {
            debug!(
                track_id = ended_id.as_u64(),
                pos, dur, "consumed ItemDidPlayToEnd (armed pre-end)"
            );
            true
        } else {
            false
        }
    }

    /// Either treat the EOF as a real end-of-track and advance, or log
    /// it as a spurious signal (decoder-failure pos stamp, crossfade
    /// fade-out on previous track).
    fn dispatch_real_or_spurious(&self, pos: f64, dur: f64) {
        /// Threshold for filtering spurious `PlayerEvent::ItemDidPlayToEnd`
        /// events emitted by crossfade fade-outs of non-current tracks.
        const ITEM_END_POSITION_TOLERANCE_SECONDS: f64 = 1.0;

        if dur > 0.0 && pos >= dur - ITEM_END_POSITION_TOLERANCE_SECONDS {
            let _ = self.advance_to_next(Transition::Crossfade);
        } else {
            debug!(pos, dur, "filtered spurious ItemDidPlayToEnd");
        }
    }

    fn drain_player_events(&self) {
        let mut rx = self
            .player_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            match rx.try_recv() {
                Ok(Envelope { event: ev, .. }) => self.process_player_event(&ev),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    fn freeze_cached_position(&self) {
        if let Some(t) = self.player.position_seconds() {
            self.write_cached_position(CachedPosition::known(t));
        }
    }

    fn handle_current_item_changed(&self) {
        let idx = self.player.current_index();
        let id = self.lock_tracks().get(idx).map(|e| e.id);
        self.write_cached_position(CachedPosition::Unknown);
        self.bus.publish(QueueEvent::CurrentTrackChanged { id });
    }

    fn handle_handover_requested(&self) {
        if self.is_paused() {
            return;
        }
        let Some(entry) = self.current() else {
            return;
        };
        self.advance_loaded_successor(entry.id, Transition::Crossfade);
    }

    fn handle_item_did_fail(&self, src: &Arc<str>) {
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position);
        let dur = snap.map_or(0.0, |s| s.duration);
        debug!(%src, pos, dur, "ItemDidFail received — track aborted mid-stream");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%src, "paused: not auto-advancing on ItemDidFail");
            return;
        }
        let ended_id = self.track_id_for_src(src);
        if self.consume_armed_advance(ended_id, pos, dur) {
            return;
        }
        let _ = self.advance_to_next(Transition::None);
    }

    /// Decide whether `ItemDidPlayToEnd` advances the queue or is
    /// filtered as a stale crossfade fade-out signal.
    ///
    /// `src` identifies the underlying audio source of the track that
    /// just hit EOF. The player only emits `TrackPlaybackStopped` from
    /// its natural-EOF path (`handle_eof`), so a non-empty `src` is the
    /// authoritative end-of-track signal: advance unconditionally
    /// (subject to crossfade pre-arm consumption).
    ///
    /// The empty-`src` arm preserves backward compatibility for
    /// pre-PR-#64 callers and acts as a defensive fallback when the
    /// player has not yet wired the src through; it falls back to the
    /// pos/dur tolerance heuristic to filter spurious events.
    fn handle_item_did_play_to_end(&self, src: &Arc<str>) {
        let snap = self.player.playback_snapshot();
        let pos = snap.map_or(0.0, |s| s.position);
        let dur = snap.map_or(0.0, |s| s.duration);
        debug!(%src, pos, dur, "ItemDidPlayToEnd received");
        if self.current().is_none() {
            self.bus.publish(QueueEvent::QueueEnded);
            return;
        }
        if self.is_paused() {
            debug!(%src, pos, dur, "paused: not auto-advancing on ItemDidPlayToEnd");
            return;
        }
        let ended_id = self.track_id_for_src(src);
        if self.consume_armed_advance(ended_id, pos, dur) {
            return;
        }
        if src.is_empty() {
            self.dispatch_real_or_spurious(pos, dur);
        } else {
            let _ = self.advance_to_next(Transition::Crossfade);
        }
    }

    /// Whether the user has paused playback.
    ///
    /// Reads the player's live rate: `pause()` (and a no-autoplay select)
    /// stores `0.0`, while `play` / `set_rate` keep it `>= MIN_PLAYBACK_RATE`.
    /// A natural end-of-track leaves the rate untouched, so this stays
    /// distinct from `is_playing()` (which drops to `false` once the arena
    /// drains at EOF). Auto-advance gates on this so a paused head freezes
    /// without blocking the genuine end-of-track advance.
    pub(super) fn is_paused(&self) -> bool {
        self.player.rate() <= 0.0
    }

    /// Start the next-track crossfade ahead of end-of-track when the
    /// remaining playtime drops below the configured crossfade window,
    /// so the two tracks actually overlap. `ItemDidPlayToEnd` alone
    /// fires after the first track is already silent — too late for a
    /// real crossfade.
    fn maybe_arm_crossfade(&self) {
        if self.is_paused() {
            return;
        }
        let crossfade = self.player.crossfade_duration();
        let view = self.playback_view();
        let (Some(dur), Some(pos), Some(entry)) = (view.duration, view.position, self.current())
        else {
            return;
        };
        let armed_for = self.read_armed_for();
        let time = super::types::PlaybackTime { dur, pos };
        if !super::types::should_arm_crossfade(time, crossfade, entry.id, armed_for) {
            return;
        }
        let transition = if crossfade > 0.0 {
            Transition::Crossfade
        } else {
            Transition::None
        };
        self.advance_loaded_successor(entry.id, transition);
    }

    /// Platform audio-route changed while playback may be active.
    ///
    /// Recreates the native output stream below the queue without
    /// changing queue state, current item, or track loading.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError`] when the underlying player cannot restart
    /// the active audio route.
    pub fn notify_audio_route_changed(&self, reason: &str) -> Result<(), QueueError> {
        self.player.invalidate_audio_route(reason)?;
        Ok(())
    }

    /// Pause playback and freeze the queue-visible head position.
    pub fn pause(&self) {
        self.player.pause();
        self.freeze_cached_position();
    }

    /// Start playback. The player consumes the current slot's resource
    /// (`items[i].take()`), so the current `Loaded` track is marked
    /// `Consumed` to keep the status truthful: a later re-select must go
    /// through the loader-respawn path, not select an emptied slot.
    pub fn play(&self) {
        let entry = {
            let guard = self.lock_tracks();
            guard
                .get(self.player.current_index())
                .map(|e| (e.id, e.status.clone()))
        };
        self.player.play();
        if let Some((id, TrackStatus::Loaded)) = entry {
            self.set_status(id, TrackStatus::Consumed);
        }
    }

    /// Single coherent read of the player's live playback state.
    ///
    /// Pollers (the FFI time thread, `snapshot`) get position, duration,
    /// decoded frontier, and the playing flag from one call instead of
    /// several separate accessors. The player-sourced fields come from one
    /// [`PlaybackSnapshot`](kithara_play::PlaybackSnapshot) via its `From`
    /// conversion; `position` is then replaced with this queue's cached,
    /// 0.0-smoothed value.
    #[must_use]
    pub fn playback_view(&self) -> PlaybackView {
        let mut view = self
            .player
            .playback_snapshot()
            .map(PlaybackView::from)
            .unwrap_or_default();
        view.position = self.position_seconds();
        view
    }

    /// Latest monotonic playback position for the current track in
    /// seconds. Updated on every [`Self::tick`]; skips transient 0.0
    /// samples the engine produces on pause/resume so downstream UIs
    /// see stable values.
    #[must_use]
    pub fn position_seconds(&self) -> Option<f64> {
        self.read_cached_position().into()
    }

    fn process_player_event(&self, ev: &Event) {
        match ev {
            Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. }) => {
                self.handle_item_did_play_to_end(src);
            }
            Event::Player(PlayerEvent::ItemDidFail { src, .. }) => {
                self.handle_item_did_fail(src);
            }
            Event::Player(PlayerEvent::CurrentItemChanged) => {
                self.handle_current_item_changed();
            }
            Event::Player(PlayerEvent::HandoverRequested) => {
                self.handle_handover_requested();
            }
            _ => {}
        }
    }

    /// Seek within the currently-playing track.
    ///
    /// Seek-hang detection is not handled here: the audio pipeline's
    /// own `#[hang_watchdog]` instrumentation (e.g. `Audio::read`,
    /// `Stream::read`, `decode_next_chunk`) already panics with a
    /// stacktrace and context dump when no progress is observed. Adding
    /// a second Queue-level watchdog would just duplicate those panics.
    ///
    /// Returns the typed [`SeekOutcome`](kithara_play::SeekOutcome) — either
    /// `Landed` with the requested target (the actual landed position is
    /// reconciled by the worker after applying the seek; this call returns
    /// the optimistic outcome) or `PastEof` if the target is beyond the
    /// known track duration.
    ///
    /// # Errors
    /// Returns [`QueueError::Play`] if the player reports a seek failure.
    pub fn seek(&self, seconds: f64) -> Result<kithara_play::SeekOutcome, QueueError> {
        // Superpowered-style resume after end-of-queue: once the last track
        // played to natural EOF the nav cursor ran off the end (`current()` is
        // `None`). Re-park the cursor to the last navigation-owned item and
        // re-announce it (`CurrentTrackChanged`) so `current()` and every
        // event-mirrored consumer (wasm/FFI/app "now playing") un-latch from
        // the ended state before the seek revives playback. During normal
        // mid-track playback `current()` is `Some`, so this is a no-op.
        if self.current().is_none() {
            let idx = { self.lock_navigation().last_selected_index() };
            if let Some(idx) = idx
                && idx < self.len()
            {
                self.lock_navigation_mut().select(idx);
                self.handle_current_item_changed();
            }
        }
        let outcome = self
            .player
            .seek_seconds(seconds)
            .map_err(QueueError::from)?;
        if let kithara_play::SeekOutcome::Landed { landed_at, .. } = outcome {
            self.write_cached_position(CachedPosition::known(landed_at.as_secs_f64()));
        }
        Ok(outcome)
    }

    /// Periodic tick: drives `PlayerImpl::tick` and drains queued engine
    /// events to act on `ItemDidPlayToEnd` (filtered) and forward
    /// `CurrentItemChanged` as
    /// [`QueueEvent::CurrentTrackChanged`](kithara_events::QueueEvent::CurrentTrackChanged).
    ///
    /// # Errors
    /// Forwards `PlayError` from `PlayerImpl::tick`.
    pub fn tick(&self) -> Result<(), QueueError> {
        self.player.tick()?;
        self.player.process_notifications();
        self.drain_player_events();
        self.update_cached_position();
        self.maybe_arm_crossfade();
        Ok(())
    }

    fn track_id_for_src(&self, src: &str) -> Option<TrackId> {
        self.lock_tracks().iter().find_map(|record| {
            let matches = match &record.source {
                TrackSource::Uri(uri) => uri == src,
                TrackSource::Config(config) => config.src.to_string() == src,
            };
            matches.then_some(record.id)
        })
    }

    fn update_cached_position(&self) {
        /// Minimum position threshold used to suppress spurious 0.0 reports
        /// on pause/resume. Values above this are considered a valid
        /// non-zero position.
        const MIN_STABLE_POSITION_SECS: f64 = 0.5;

        if self.is_paused() {
            return;
        }

        let Some(t) = self.player.position_seconds() else {
            return;
        };
        let prev = Option::<f64>::from(self.read_cached_position());
        if t == 0.0 && prev.is_some_and(|p| p > MIN_STABLE_POSITION_SECS) {
            return;
        }
        self.write_cached_position(CachedPosition::known(t));
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::TrackId;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::queue::{
        state::tests::make_queue,
        types::{CrossfadeArm, PlaybackTime, should_arm_crossfade},
    };

    #[kithara::test(tokio)]
    async fn spurious_item_did_play_to_end_is_filtered() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");

        queue
            .player
            .bus()
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd {
                src: Arc::from(""),
                item_id: None,
            }));

        queue
            .tick()
            .expect("BUG: tick returned error in test setup");

        let nav_idx = queue.lock_navigation().current_index();
        assert_eq!(nav_idx, None, "navigation must not have advanced");
    }

    #[kithara::test(tokio)]
    async fn eof_after_queue_end_does_not_restart_from_first_track() {
        let queue = make_queue();
        let _a = queue.register_for_test();
        let b = queue.register_for_test();
        queue.lock_navigation_mut().select(1);
        queue.lock_navigation_mut().finish();
        let mut rx = queue.subscribe();

        queue
            .player
            .bus()
            .publish(Event::Player(PlayerEvent::ItemDidPlayToEnd {
                src: Arc::from(format!("test://memory/{}", b.as_u64())),
                item_id: None,
            }));

        queue
            .tick()
            .expect("BUG: tick returned error in test setup");

        let nav_idx = queue.lock_navigation().current_index();
        assert_eq!(nav_idx, None, "stale EOF must not restart the queue");
        let saw_ended = crate::queue::state::tests::wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::QueueEnded),
            200,
        )
        .await;
        assert!(saw_ended, "stale EOF should re-announce QueueEnded");
    }

    #[kithara::test]
    #[case::remaining_equals_crossfade(157.0, 162.0, 5.0, TrackId(1), CrossfadeArm::Disarmed, true)]
    #[case::remaining_below_crossfade(160.0, 162.0, 5.0, TrackId(1), CrossfadeArm::Disarmed, true)]
    #[case::far_from_end(100.0, 162.0, 5.0, TrackId(1), CrossfadeArm::Disarmed, false)]
    #[case::already_armed_for_same_track(
        160.0,
        162.0,
        5.0,
        TrackId(1),
        CrossfadeArm::armed(TrackId(1)),
        false
    )]
    #[case::armed_for_different_track_still_arms(
        160.0,
        162.0,
        5.0,
        TrackId(1),
        CrossfadeArm::armed(TrackId(0)),
        true
    )]
    #[case::crossfade_zero_at_tail_no_pre_arm(
        161.9,
        162.0,
        0.0,
        TrackId(1),
        CrossfadeArm::Disarmed,
        false
    )]
    #[case::crossfade_zero_quiet_middle(
        161.0,
        162.0,
        0.0,
        TrackId(1),
        CrossfadeArm::Disarmed,
        false
    )]
    #[case::zero_position_rejected(0.0, 162.0, 5.0, TrackId(1), CrossfadeArm::Disarmed, false)]
    #[case::zero_duration_rejected(10.0, 0.0, 5.0, TrackId(1), CrossfadeArm::Disarmed, false)]
    fn should_arm_crossfade_cases(
        #[case] pos: f64,
        #[case] dur: f64,
        #[case] crossfade: f32,
        #[case] current_id: TrackId,
        #[case] armed_for: CrossfadeArm,
        #[case] expected: bool,
    ) {
        assert_eq!(
            should_arm_crossfade(PlaybackTime { dur, pos }, crossfade, current_id, armed_for),
            expected
        );
    }
}
