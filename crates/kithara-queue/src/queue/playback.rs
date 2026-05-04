//! Runtime tick: position cache, crossfade arming, engine event drain
//! (`ItemDidPlayToEnd` / `CurrentItemChanged`), and `seek`.

use std::sync::PoisonError;

use kithara_events::{Event, PlayerEvent, QueueEvent};
use tokio::sync::broadcast::error::TryRecvError;
use tracing::debug;

use super::{Queue, types::Transition};
use crate::error::QueueError;

impl Queue {
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
        let armed_for = self.read_armed_for();
        if !super::types::should_arm_crossfade(pos, dur, crossfade, entry.id, armed_for) {
            return;
        }
        self.write_armed_for(Some(entry.id));
        let transition = if crossfade > 0.0 {
            Transition::Crossfade
        } else {
            Transition::None
        };
        let _ = self.advance_to_next(transition);
    }

    /// Latest monotonic playback position for the current track in
    /// seconds. Updated on every [`Self::tick`]; skips transient 0.0
    /// samples the engine produces on pause/resume so downstream UIs
    /// see stable values.
    #[must_use]
    pub fn position_seconds(&self) -> Option<f64> {
        self.read_cached_position()
    }

    fn process_player_event(&self, ev: &Event) {
        match ev {
            Event::Player(PlayerEvent::ItemDidPlayToEnd { src, .. }) => {
                self.handle_item_did_play_to_end(src);
            }
            Event::Player(PlayerEvent::CurrentItemChanged) => {
                self.handle_current_item_changed();
            }
            _ => {}
        }
    }

    fn handle_current_item_changed(&self) {
        let idx = self.player.current_index();
        let id = self.lock_tracks().get(idx).map(|e| e.id);
        self.write_cached_position(None);
        self.bus.publish(QueueEvent::CurrentTrackChanged { id });
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
    fn handle_item_did_play_to_end(&self, src: &std::sync::Arc<str>) {
        let pos = self.player.position_seconds().unwrap_or(0.0);
        let dur = self.player.duration_seconds().unwrap_or(0.0);
        debug!(%src, pos, dur, "ItemDidPlayToEnd received");
        if self.consume_armed_advance(pos, dur) {
            return;
        }
        if src.is_empty() {
            self.dispatch_real_or_spurious(pos, dur);
        } else {
            let _ = self.advance_to_next(Transition::Crossfade);
        }
    }

    /// If an advance was already armed from `tick()`, consume it and
    /// return `true` — the engine's trailing `ItemDidPlayToEnd` for
    /// the same track must not advance again.
    fn consume_armed_advance(&self, pos: f64, dur: f64) -> bool {
        if self.take_armed_for().is_some() {
            debug!(pos, dur, "consumed ItemDidPlayToEnd (armed pre-end)");
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
        self.player.seek_seconds(seconds).map_err(QueueError::from)
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

        let Some(t) = self.player.position_seconds() else {
            return;
        };
        // Engine briefly reports 0.0 on pause/resume; keep the last
        // sane value so slider bindings don't flash back to the start.
        if t == 0.0
            && self
                .read_cached_position()
                .is_some_and(|prev| prev > MIN_STABLE_POSITION_SECS)
        {
            return;
        }
        self.write_cached_position(Some(t));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_events::TrackId;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::queue::{state::tests::make_queue, types::should_arm_crossfade};

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

        queue.tick().expect("tick");

        let nav_idx = queue.lock_navigation().current_index();
        assert_eq!(nav_idx, None, "navigation must not have advanced");
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
    // cf=0: production replaced the END_PROXIMITY_SECONDS pre-arm window
    // with sample-accurate handover via the audio thread's
    // `TrackPlaybackStopped` notification (PR #64). The pre-arm path
    // returns `false` for cf=0 across all positions; the next-track
    // promotion happens through the player processor instead.
    #[case::crossfade_zero_at_tail_no_pre_arm(161.9, 162.0, 0.0, TrackId(1), None, false)]
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
}
