use kithara_events::TrackStatus;

use super::{
    Queue,
    types::{CachedPosition, PendingSelect, PlaybackView, Transition},
};
use crate::error::QueueError;

impl Queue {
    fn freeze_cached_position(&self) {
        if let Some(t) = self.player.position_seconds() {
            self.write_cached_position(CachedPosition::known(t));
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
    ///
    /// Which item was consumed is read back from the player rather than
    /// inferred from a status snapshot taken beforehand. `play()` starts the
    /// audio engine before it loads, and a load completing inside that window
    /// (130-400 ms against a real device) fills the slot and is picked up by
    /// the same call — a track that was not yet `Loaded` when the snapshot was
    /// taken. Recording nothing then leaves `Loaded` standing over an emptied
    /// slot, and every later select of that track is rejected with
    /// `PlayError::ItemConsumed`.
    ///
    /// When the load has *not* landed yet the slot is empty, the player
    /// planted nothing, and the request would otherwise be lost. `play()` is
    /// the intent "start this track", so it is recorded as a pending select on
    /// the loading track: `spawn_apply_after_load` applies it the moment the
    /// resource arrives. Without this the window in which `play()` wins the
    /// race is silent forever — and that window is the normal case, because
    /// only the process's very first `play()` is slowed by starting the output
    /// stream.
    ///
    /// The reconciliation runs under the selection lock so a concurrent
    /// `spawn_apply_after_load` cannot publish `Loaded` on top of the slot
    /// this call just emptied.
    pub fn play(&self) {
        self.player.play();

        let _apply = self.lock_select_apply();
        let index = self.player.current_index();
        if self.player.item_has_resource(index) {
            return;
        }
        let current = {
            let guard = self.lock_tracks();
            guard
                .get(index)
                .map(|entry| (entry.id, entry.status.clone()))
        };
        let Some((id, status)) = current else {
            return;
        };
        match status {
            TrackStatus::Loaded => self.set_status(id, TrackStatus::Consumed),
            TrackStatus::Pending | TrackStatus::Loading | TrackStatus::Slow => {
                self.override_pending_select(PendingSelect {
                    id,
                    transition: Transition::None,
                });
                self.promote_pending_load(id);
            }
            _ => {}
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
    use kithara_events::{Event, PlayerEvent, QueueEvent, TrackId};
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

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
