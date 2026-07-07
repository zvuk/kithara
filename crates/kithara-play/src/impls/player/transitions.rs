use std::sync::{Arc, atomic::Ordering};

use kithara_audio::SeekOutcome;
use kithara_platform::time::Duration;
use tracing::{debug, warn};

use super::core::PlayerImpl;
use crate::{
    error::PlayError,
    events::PlayerEvent,
    impls::{player_processor::PlayerCmd, player_track::TrackTransition},
    types::PlayerStatus,
};

/// How a [`PlayerImpl::select_item_with_crossfade`] transition behaves:
/// whether to `autoplay` the selected item and the `crossfade_seconds`
/// fade applied for this one transition.
#[derive(Debug, Clone, Copy)]
pub struct SelectTransition {
    pub autoplay: bool,
    pub crossfade_seconds: f32,
}

impl PlayerImpl {
    /// Apply autoplay: resume at the default rate (and move to `Playing`) or
    /// hold at rate 0 (and move to `Paused`).
    fn apply_autoplay(&self, autoplay: bool) {
        if autoplay {
            let default_rate = self.default_rate();
            self.core.rate.store(default_rate, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(false));
            self.enter_playing();
            self.core
                .bus
                .publish(PlayerEvent::RateChanged { rate: default_rate });
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            self.core.rate.store(0.0, Ordering::Relaxed);
            let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
            self.enter_paused();
            self.core
                .bus
                .publish(PlayerEvent::RateChanged { rate: 0.0 });
        }
    }

    /// Load the current queue item into the active slot.
    ///
    /// Takes the resource out of the queue (replacing with `None`), wraps it
    /// in `PlayerResource`, and sends `LoadTrack` + `FadeIn` to the processor.
    fn load_current_item(&self) {
        let index = self.core.current_index.load(Ordering::Relaxed);
        if let Some((src, duration_seconds)) = self.enqueue_to_processor(index) {
            self.publish_current_track_snapshot(duration_seconds);
            self.start_playback(src);
        }
    }

    /// Pause playback (sets rate to 0.0).
    pub fn pause(&self) {
        self.core.rate.store(0.0, Ordering::Relaxed);
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        self.enter_paused();
        self.core
            .bus
            .publish(PlayerEvent::RateChanged { rate: 0.0 });
        debug!(phase = ?self.phase_kind(), "pause");
    }

    /// Start playback at the configured default rate.
    pub fn play(&self) {
        let rate = self.default_rate().max(Self::MIN_PLAYBACK_RATE);
        self.core.rate.store(rate, Ordering::Relaxed);
        self.core.config.timestretch.set_speed(rate);

        if let Err(e) = self.ensure_engine_started() {
            warn!(?e, "failed to start engine");
            return;
        }
        if let Err(e) = self.ensure_slot() {
            warn!(?e, "failed to allocate slot");
            return;
        }

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(self.crossfade_duration()));
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));
        self.load_current_item();
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(rate));
        let _ = self.send_to_slot(PlayerCmd::SetPaused(false));

        self.enter_playing();
        self.set_status(PlayerStatus::ReadyToPlay);
        // Resuming the same item is not a track change; announce gates on it.
        self.announce_current_item(self.core.current_index.load(Ordering::Relaxed));
        self.core.bus.publish(PlayerEvent::RateChanged { rate });
        debug!(rate, phase = ?self.phase_kind(), "play");
    }

    /// Seek active tracks to position in seconds.
    ///
    /// Returns the typed [`SeekOutcome`] — either `Landed` with the requested
    /// target (the actual landed position is committed asynchronously by the
    /// worker thread; this call returns the optimistic outcome) or `PastEof`
    /// when the target is past the current track's known duration.
    pub fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        let Some(slot_id) = self.slot() else {
            return Err(PlayError::NotReady);
        };

        let Some(playback) = self.core.engine.slot_playback(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };

        let seek_epoch = playback.next_seek_epoch();
        playback.seek_epoch.store(seek_epoch, Ordering::SeqCst);

        let target_secs = seconds.max(0.0);
        let target = Duration::from_secs_f64(target_secs);
        let outcome = match self.duration_seconds() {
            Some(dur) if target_secs >= dur => SeekOutcome::PastEof {
                target,
                duration: Duration::from_secs_f64(dur),
            },
            _ => SeekOutcome::Landed {
                target,
                landed_at: target,
            },
        };

        self.send_to_slot(PlayerCmd::Seek {
            seek_epoch,
            seconds: target_secs,
        })?;

        if matches!(outcome, SeekOutcome::Landed { .. }) {
            playback.position.store(target_secs, Ordering::Relaxed);
        }

        Ok(outcome)
    }

    /// Select and load a queue item by index, using the configured
    /// crossfade duration for the transition.
    pub fn select_item(&self, index: usize, autoplay: bool) -> Result<(), PlayError> {
        self.select_item_with_crossfade(
            index,
            SelectTransition {
                autoplay,
                crossfade_seconds: self.crossfade_duration(),
            },
        )
    }

    /// Select and load a queue item by index, applying an explicit
    /// crossfade duration for this one transition only.
    ///
    /// Does not mutate the player-configured crossfade — subsequent
    /// calls to [`select_item`](Self::select_item) fall back to
    /// [`crossfade_duration`](Self::crossfade_duration). Pass `0.0` for an
    /// immediate cut (no fade); matches `AVQueuePlayer`'s manual-selection
    /// idiom.
    pub fn select_item_with_crossfade(
        &self,
        index: usize,
        transition: SelectTransition,
    ) -> Result<(), PlayError> {
        let SelectTransition {
            autoplay,
            crossfade_seconds,
        } = transition;
        let items_len = self.item_count();
        if index >= items_len {
            return Err(PlayError::Internal(format!(
                "item index out of range: {index} (len: {items_len})"
            )));
        }

        // Re-selecting the already-current item: its resource was consumed by
        // the load that made it current and now lives in the processor (it is
        // the playing track). Like the armed case, an emptied slot here is
        // expected, not stale — so the consumed-slot guard must not fire and we
        // take the no-reload path (no `enqueue_to_processor`, no re-announce).
        // Gated on `last_announced_index` so it covers only an item already
        // loaded as current, not a fresh select of `current_index` whose
        // resource genuinely still sits in the slot.
        let reselecting_current = index == self.core.current_index.load(Ordering::Relaxed)
            && index == self.core.last_announced_index.load(Ordering::Relaxed);

        let armed_for_index = self
            .phase
            .lock()
            .pending()
            .is_some_and(|p| !p.state.activated() && p.index == index);
        // An armed (or current-and-loaded) item's resource already lives in the
        // processor; otherwise the slot must still hold one —
        // `enqueue_to_processor` takes it out, so an emptied slot means the
        // caller's view of the item is stale. Fail before any bookkeeping so
        // the UI cannot drift from the audio.
        if !armed_for_index
            && !reselecting_current
            && self
                .core
                .items
                .lock()
                .get(index)
                .is_none_or(Option::is_none)
        {
            return Err(PlayError::Internal(format!(
                "item {index} has no resource (already consumed)"
            )));
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(crossfade_seconds));
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));

        if armed_for_index {
            self.commit_next(index)?;
        } else if !reselecting_current {
            self.unarm_next_internal(Some(index));
            self.core.current_index.store(index, Ordering::Relaxed);
            self.load_current_item();
            self.announce_current_item(index);
        }

        self.apply_autoplay(autoplay);
        Ok(())
    }

    pub(crate) fn start_playback(&self, src: Arc<str>) {
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(src)));
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::impls::player::PlayerConfig;

    #[kithara::test]
    fn seek_seconds_without_slot_returns_not_ready() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player.seek_seconds(1.0).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn select_item_out_of_range_returns_internal() {
        let player = PlayerImpl::new(PlayerConfig::default());
        let err = player
            .select_item_with_crossfade(
                5,
                SelectTransition {
                    autoplay: false,
                    crossfade_seconds: 0.0,
                },
            )
            .expect_err("must error");
        assert!(matches!(err, PlayError::Internal(_)));
    }

    /// `enqueue_to_processor` takes the resource out of the slot, so a
    /// select against an emptied (consumed) slot has nothing to load: it
    /// must fail loudly instead of moving `current_index` / announcing
    /// `CurrentItemChanged` while the old audio keeps playing.
    #[kithara::test]
    fn select_item_on_consumed_slot_errors_without_bookkeeping() {
        let player = PlayerImpl::new(PlayerConfig::default());
        player.reserve_slots(2);
        let result = player.select_item_with_crossfade(
            1,
            SelectTransition {
                autoplay: false,
                crossfade_seconds: 0.0,
            },
        );
        assert!(result.is_err(), "selecting an emptied slot must fail");
        assert_eq!(
            player.current_index(),
            0,
            "bookkeeping must not move on a failed select"
        );
    }
}
