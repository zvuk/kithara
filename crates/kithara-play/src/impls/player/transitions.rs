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
        if let Some(src) = self.enqueue_to_processor(index) {
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
        self.core
            .playback_rate_shared
            .store(rate, Ordering::Relaxed);

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
        self.core.bus.publish(PlayerEvent::CurrentItemChanged);
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

        let Some(shared_state) = self.core.engine.slot_shared_state(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };

        let seek_epoch = shared_state.next_seek_epoch();
        shared_state.seek_epoch.store(seek_epoch, Ordering::SeqCst);

        let target_secs = seconds.max(0.0);
        let target = Duration::from_secs_f64(target_secs);

        self.send_to_slot(PlayerCmd::Seek {
            seek_epoch,
            seconds: target_secs,
        })?;

        Ok(match self.duration_seconds() {
            Some(dur) if target_secs >= dur => SeekOutcome::PastEof {
                target,
                duration: Duration::from_secs_f64(dur),
            },
            _ => SeekOutcome::Landed {
                target,
                landed_at: target,
            },
        })
    }

    /// Select and load a queue item by index, using the configured
    /// crossfade duration for the transition.
    pub fn select_item(&self, index: usize, autoplay: bool) -> Result<(), PlayError> {
        self.select_item_with_crossfade(index, autoplay, self.crossfade_duration())
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
        autoplay: bool,
        crossfade_seconds: f32,
    ) -> Result<(), PlayError> {
        let items_len = self.item_count();
        if index >= items_len {
            return Err(PlayError::Internal(format!(
                "item index out of range: {index} (len: {items_len})"
            )));
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(crossfade_seconds));
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));

        let armed_for_index = self
            .phase
            .lock_sync()
            .pending()
            .is_some_and(|p| !p.state.activated() && p.index == index);
        if armed_for_index {
            self.commit_next(index)?;
        } else {
            self.unarm_next_internal(Some(index));
            self.core.current_index.store(index, Ordering::Relaxed);
            self.core.bus.publish(PlayerEvent::CurrentItemChanged);
            self.load_current_item();
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
            .select_item_with_crossfade(5, false, 0.0)
            .expect_err("must error");
        assert!(matches!(err, PlayError::Internal(_)));
    }
}
