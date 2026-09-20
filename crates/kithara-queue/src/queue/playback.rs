use kithara_bufpool::HasPool;
use kithara_play::{PlayError, SeekOutcome, SessionDuckingMode};
use smallvec::SmallVec;

use super::{
    QueueControl,
    types::{CachedPosition, PendingSelect, PlaybackView, SelectPhase, Transition},
};
use crate::{
    error::QueueError,
    event::{AdvanceReason, TrackStatus},
};

impl<S> QueueControl<S>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    fn freeze_cached_position(&self) {
        if let Some(t) = self.player.position_seconds() {
            self.write_cached_position(CachedPosition::known(t));
        }
    }

    /// Whether the user has paused playback.
    ///
    /// Reads the Player's explicit paused phase, not its effective rate or
    /// live output state: both become inactive at natural EOF without turning
    /// that EOF into a user pause.
    pub(super) fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    /// Start the next-track crossfade ahead of end-of-track when the
    /// remaining playtime drops below the configured crossfade window,
    /// so the two tracks actually overlap. `ItemDidPlayToEnd` alone
    /// fires after the first track is already silent — too late for a
    /// real crossfade.
    pub(super) fn maybe_arm_crossfade(&self) {
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
        self.with_open_result(|queue| queue.player.invalidate_audio_route(reason))?;
        Ok(())
    }

    /// Lower or restore the whole session output under a competing sound,
    /// such as a call or a navigation prompt.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError`] when the session rejects the change.
    pub fn set_session_ducking(&self, mode: SessionDuckingMode) -> Result<(), QueueError> {
        self.with_open_result(|queue| queue.player.set_session_ducking(mode))?;
        Ok(())
    }

    /// Pause playback and freeze the queue-visible head position.
    pub fn pause(&self) {
        self.command(|queue| {
            queue.player.pause();
            let mut phase = queue.lock_pending_select_mut();
            if let SelectPhase::Pending(mut pending) = *phase {
                pending.playback = kithara_play::SelectionPlayback::Pause;
                *phase = SelectPhase::Pending(pending);
            }
            drop(phase);
            queue.freeze_cached_position();
        });
    }

    /// Starts playback, marking a consumed slot or retaining the selection until loading finishes.
    /// Reconciliation is serialized with load completion.
    pub fn play(&self) {
        self.command(Self::play_inner);
    }

    fn play_inner(&self) {
        let mut phase = self.lock_pending_select_mut();
        if let SelectPhase::Pending(mut pending) = *phase {
            pending.playback = kithara_play::SelectionPlayback::Play;
            *phase = SelectPhase::Pending(pending);
        }
        drop(phase);
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
                if matches!(*self.lock_pending_select_mut(), SelectPhase::Pending(_)) {
                    return;
                }
                self.override_pending_select(PendingSelect {
                    id,
                    settings: Transition::None.settings(self.crossfade_settings()),
                    playback: kithara_play::SelectionPlayback::Play,
                    reason: AdvanceReason::UserSelect,
                });
                self.promote_pending_load(id);
            }
            TrackStatus::Failed(_) | TrackStatus::Consumed | TrackStatus::Cancelled => {}
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

    pub(super) fn seek_player(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        self.with_open_result(|queue| queue.seek_player_inner(seconds))
    }

    fn seek_player_inner(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        // WHY: Superpowered-style resume after end-of-queue: once the last track played to natural EOF the nav cursor ran off the end
        // (`current()` is `None`).
        if self.current().is_none() {
            let id = { self.lock_navigation().last_selected() };
            if let Some(id) = id {
                let ids = self
                    .tracks()
                    .into_iter()
                    .map(|track| track.id)
                    .collect::<SmallVec<[_; 16]>>();
                self.lock_navigation_mut().select(id, &ids);
                self.handle_current_item_changed();
            }
        }
        let outcome = self.player.seek_seconds(seconds)?;
        if let SeekOutcome::Landed { landed_at, .. } = outcome {
            self.write_cached_position(CachedPosition::known(landed_at.as_secs_f64()));
        }
        Ok(outcome)
    }

    pub(super) fn tick_player(&self) -> Result<(), PlayError> {
        self.with_open_result(Self::tick_player_inner)
    }

    fn tick_player_inner(&self) -> Result<(), PlayError> {
        self.player.tick()?;
        self.observe_player_tick();
        Ok(())
    }

    /// Folds what one player tick published into the queue's view.
    pub(super) fn observe_player_tick(&self) {
        self.player.process_notifications();
        self.drain_player_events();
        self.update_cached_position();
        self.maybe_arm_crossfade();
    }

    pub(super) fn update_cached_position(&self) {
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

    delegate::delegate! {
        to self {
            /// Latest monotonic playback position for the current track in
            /// seconds. Updated on every [`Self::tick`]; skips transient 0.0
            /// samples the engine produces on pause/resume so downstream UIs
            /// see stable values.
            #[must_use]
            #[into]
            #[call(read_cached_position)]
            pub fn position_seconds(&self) -> Option<f64>;

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
            #[expr($.map_err(QueueError::from))]
            #[call(seek_player)]
            pub fn seek(&self, seconds: f64) -> Result<SeekOutcome, QueueError>;

            /// Periodic tick: drives `PlayerImpl::tick` and drains queued engine
            /// events to act on `ItemDidPlayToEnd` (filtered) and forward
            /// `CurrentItemChanged` as
            /// [`QueueEvent::CurrentTrackChanged`](crate::event::QueueEvent::CurrentTrackChanged).
            ///
            /// # Errors
            /// Forwards `PlayError` from `PlayerImpl::tick`.
            #[expr($.map_err(QueueError::from))]
            #[call(tick_player)]
            pub fn tick(&self) -> Result<(), QueueError>;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{SlotId, TrackId};
    use kithara_platform::sync::Arc;
    use kithara_play::{ItemRole, PlayerEvent, TrackRef};
    use kithara_test_utils::kithara;

    use crate::{
        event::QueueEvent,
        queue::{
            state::tests::make_queue,
            types::{CrossfadeArm, PlaybackTime, should_arm_crossfade},
        },
    };

    #[kithara::test(tokio)]
    async fn spurious_item_did_play_to_end_is_filtered() {
        let queue = make_queue();
        let _a = queue.append("https://example.com/a.mp3");
        let _b = queue.append("https://example.com/b.mp3");

        queue.player.bus().publish(PlayerEvent::ItemDidPlayToEnd {
            item: ItemRole::Leading(TrackRef::new(
                TrackId::allocate(),
                SlotId::new(0),
                Arc::from(""),
            )),
        });

        queue
            .tick()
            .expect("BUG: tick returned error in test setup");

        assert_eq!(
            queue.lock_navigation().current(),
            None,
            "navigation must not have advanced"
        );
    }

    #[kithara::test(tokio)]
    async fn eof_after_queue_end_does_not_restart_from_first_track() {
        let queue = make_queue();
        let a = queue
            .append("https://example.com/a.mp3")
            .expect("open queue accepts a track");
        let b = queue
            .append("https://example.com/b.mp3")
            .expect("open queue accepts a track");
        queue.lock_navigation_mut().select(b, &[a, b]);
        queue.lock_navigation_mut().finish();
        let mut rx = queue.subscribe();

        queue.player.bus().publish(PlayerEvent::ItemDidPlayToEnd {
            item: ItemRole::Leading(TrackRef::new(
                b,
                SlotId::new(0),
                Arc::from(format!("test://memory/{}", b.as_u64())),
            )),
        });

        queue
            .tick()
            .expect("BUG: tick returned error in test setup");

        assert_eq!(
            queue.lock_navigation().current(),
            None,
            "stale EOF must not restart the queue"
        );
        let saw_ended = crate::queue::state::tests::wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::QueueEnded),
            200,
        )
        .await;
        assert!(!saw_ended, "stale EOF must not duplicate QueueEnded");
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
