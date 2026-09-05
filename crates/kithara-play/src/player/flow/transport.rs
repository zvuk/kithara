use std::sync::atomic::Ordering;

use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::time::Duration;
use tracing::{debug, warn};

#[cfg(test)]
use super::super::core::PlayerImpl;
use super::super::core::PlayerRuntime;
use crate::{
    api::{PlayerStatus, TrackId},
    bridge::{PlayerCmd, TrackTransition},
    error::PlayError,
};

/// How a [`PlayerImpl::select_item_with_crossfade`] transition behaves:
/// whether to `autoplay` the selected item and the `crossfade_seconds`
/// fade applied for this one transition.
#[derive(Debug, Clone, Copy)]
pub struct SelectTransition {
    pub autoplay: bool,
    pub crossfade_seconds: f32,
}

impl<S> PlayerRuntime<S>
where
    S: HasPool<f32>,
{
    fn apply_autoplay(&self, autoplay: bool) {
        if autoplay {
            self.set_rate(self.default_rate());
            let _ = self.send_to_slot(PlayerCmd::SetPaused(false));
            self.enter_playing();
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
            self.enter_paused();
        }
    }

    /// Ensure the audio engine is started.
    pub fn ensure_engine_started(&self) -> Result<(), PlayError> {
        if self.core.engine.is_running() {
            return Ok(());
        }
        match self.core.engine.start() {
            Ok(()) | Err(PlayError::EngineAlreadyRunning) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Load the current queue item into the active slot.
    ///
    /// Takes the resource out of the queue (replacing with `None`), wraps it
    /// in `PlayerResource`, and sends `LoadTrack` + `FadeIn` to the processor.
    ///
    /// `false` means the slot held no resource, so nothing reached the
    /// processor and the item is not current.
    fn load_current_item(&self) -> Result<bool, PlayError> {
        let index = self.current_index();
        let Some((item_id, _src, duration_seconds)) = self.enqueue_to_processor(index)? else {
            return Ok(false);
        };
        self.publish_current_track_snapshot(duration_seconds);
        self.start_playback(item_id);
        Ok(true)
    }

    /// Pause playback. The effective rate becomes `0.0` when RT applies the command.
    pub fn pause(&self) {
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        self.enter_paused();
        debug!(phase = ?self.phase_kind(), "pause");
    }

    /// Start playback from the configured default-rate target.
    pub fn play(&self) {
        let rate = self.default_rate().max(Self::MIN_PLAYBACK_RATE);
        self.core.warp.stretch().set_speed(rate);

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
        let loaded = self.load_current_item().unwrap_or_else(|error| {
            warn!(%error, "failed to allocate track playback buffers");
            false
        });
        let _ = self.send_to_slot(PlayerCmd::SetPlaybackRate(rate));
        let _ = self.send_to_slot(PlayerCmd::SetPaused(false));

        self.enter_playing();
        self.set_status(PlayerStatus::ReadyToPlay);
        // WHY: Resuming the same item is not a track change; announce gates on it. An empty slot means the item's load is still in flight:
        // announcing it would mark the index current, and the select that plants the arriving resource would then take
        // `select_item_with_crossfade`'s reselecting-current path and never enqueue it.
        if loaded {
            self.announce_current_item(self.current_index());
        }
        debug!(rate, phase = ?self.phase_kind(), "play");
    }

    /// Seek active tracks to position in seconds.
    ///
    /// Returns the typed [`SeekOutcome`] — either `Landed` with the requested
    /// target (the actual landed position is committed asynchronously by the
    /// worker thread; this call returns the optimistic outcome) or `PastEof`
    /// when the target is past the current track's known duration.
    ///
    /// The outcome is classified against the duration observed *before*
    /// `begin_slot_seek` rebases the source. Reading it afterwards judges the
    /// request against a duration the request itself perturbed: the audio
    /// thread can render a block off the rebased source in that window and
    /// republish a shorter `PlaybackShared::duration`, turning an in-range
    /// target into a spurious `PastEof`.
    pub fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        let Some(slot_id) = self.slot() else {
            return Err(PlayError::NotReady);
        };

        let Some(playback) = self.core.engine.slot_playback(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };

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

        // WHY: The `fetch_add` inside is the publication: storing the returned value back would let two concurrent seeks reinstate the older
        // epoch.
        let seek_epoch = playback.next_seek_epoch();

        // WHY: Begin here, on the control thread: minting the source epoch publishes an event and wakes the decode worker, both of which
        // take locks.
        self.core.engine.begin_slot_seek(slot_id, target);

        if let Err(err) = self.send_to_slot(PlayerCmd::Seek {
            seek_epoch,
            seconds: target_secs,
        }) {
            // WHY: Nothing will carry the re-base now, and the processor holds a track's natural end while a published seek outranks it.
            playback.withdraw_seek_epoch(seek_epoch);
            return Err(err);
        }

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
            return Err(PlayError::IndexOutOfRange {
                index,
                len: items_len,
            });
        }

        // WHY: Re-selecting the already-current item: its resource was consumed by the load that made it current and now lives in the
        // processor (it is the playing track).
        let reselecting_current =
            index == self.core.items.current_index() && self.core.items.is_announced(index);
        let has_resource = self.core.items.has_resource(index);

        let armed_for_index = self
            .phase
            .lock()
            .pending()
            .is_some_and(|p| !p.state.activated() && p.index == index);
        // WHY: An armed (or current-and-loaded) item's resource already lives in the processor; otherwise the slot must still hold one -
        // `enqueue_to_processor` takes it out, so an emptied slot means the caller's view of the item is stale.
        if !armed_for_index && !reselecting_current && !has_resource {
            return Err(PlayError::ItemConsumed { index });
        }

        if autoplay {
            self.core.warp.stretch().set_speed(self.default_rate());
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;

        let _ = self.send_to_slot(PlayerCmd::SetFadeDuration(crossfade_seconds));
        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));

        if armed_for_index {
            self.commit_next(index)?;
        } else if !reselecting_current {
            self.unarm_next_internal(Some(index));
            self.core.items.set_current(index);
            self.load_current_item()?;
            self.announce_current_item(index);
        }

        self.apply_autoplay(autoplay);
        Ok(())
    }

    pub(crate) fn start_playback(&self, item_id: TrackId) {
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(item_id)));
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig,
        player::PlayerConfig,
        session::testing,
        test_pools::{TestPools, pools},
    };

    fn player() -> PlayerImpl<TestPools> {
        let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
        PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(testing::TEST_SAMPLE_RATE)
                .worker(worker)
                .session(testing::test_session())
                .build(),
        )
    }

    #[kithara::test]
    fn seek_seconds_without_slot_returns_not_ready() {
        let player = player();
        let err = player.seek_seconds(1.0).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn select_item_out_of_range_returns_typed_error() {
        let player = player();
        let err = player
            .select_item_with_crossfade(
                5,
                SelectTransition {
                    autoplay: false,
                    crossfade_seconds: 0.0,
                },
            )
            .expect_err("must error");
        assert!(matches!(
            err,
            PlayError::IndexOutOfRange { index: 5, len: 0 }
        ));
    }

    /// `enqueue_to_processor` takes the resource out of the slot, so a
    /// select against an emptied (consumed) slot has nothing to load: it
    /// must fail loudly instead of moving the playlist current index / announcing
    /// `CurrentItemChanged` while the old audio keeps playing.
    #[kithara::test]
    fn select_item_on_consumed_slot_errors_without_bookkeeping() {
        let player = player();
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
