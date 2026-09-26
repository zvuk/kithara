use std::sync::atomic::Ordering;

use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::time::Duration;
use tracing::{debug, warn};

use super::super::core::PlayerRuntime;
use crate::{
    api::{CrossfadeSettings, PlayerStatus, SelectionPlayback, TrackId},
    bridge::{PlayerCmd, TrackTransition},
    error::PlayError,
};

/// Captured transport intent and complete fade profile for one selection.
#[derive(Debug, Clone, Copy)]
pub struct SelectTransition {
    pub crossfade: CrossfadeSettings,
    pub playback: SelectionPlayback,
}

impl<S> PlayerRuntime<S>
where
    S: HasPool<f32>,
{
    fn apply_playback(&self, playback: SelectionPlayback) {
        if playback == SelectionPlayback::Play {
            let _ = self.send_to_slot(PlayerCmd::SetPaused(false));
            self.enter_playing();
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
            self.enter_paused();
        }
    }

    /// Place the freshly-loaded track at the position handed over before it
    /// existed. Must follow [`Self::start_playback`]: a fade-in re-bases a
    /// track that is past its head, which would undo the seek.
    fn apply_start_position(&self) {
        let Some(target) = self.core.start_position.lock().take() else {
            return;
        };
        let seconds = target.as_secs_f64();
        if let Err(e) = self.seek_seconds(seconds) {
            warn!(?e, seconds, "start position rejected by the loaded track");
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
        self.start_playback(item_id, duration_seconds);
        self.apply_start_position();
        Ok(true)
    }

    fn load_current_item_with(&self, crossfade: CrossfadeSettings) -> Result<bool, PlayError> {
        let index = self.current_index();
        let Some((item_id, _src, duration_seconds)) = self.enqueue_to_processor(index)? else {
            return Ok(false);
        };
        self.start_playback_with(item_id, duration_seconds, crossfade);
        self.apply_start_position();
        Ok(true)
    }

    /// Pause playback. The effective rate becomes `0.0` when RT applies the command.
    pub fn pause(&self) {
        let _ = self.send_to_slot(PlayerCmd::SetPaused(true));
        self.enter_paused();
        debug!(phase = ?self.phase_kind(), "pause");
    }

    /// Start playback from the configured default-rate target.
    ///
    /// Announces the current item only once a slot is loaded; announcing while the load is still in
    /// flight would mark the index current early and make a later select skip re-enqueuing the
    /// arriving resource.
    pub fn play(&self) {
        let rate = self.core.warp.stretch().speed();

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
        let _ = self.send_to_slot(PlayerCmd::SetPaused(false));

        self.enter_playing();
        self.set_status(PlayerStatus::ReadyToPlay);
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
    ///
    /// A seek that arrives before the player holds a slot is kept as the
    /// current item's start position and applied by the load that starts it,
    /// so a position handed over at queue-seeding time is where playback
    /// begins.
    pub fn seek_seconds(&self, seconds: f64) -> Result<SeekOutcome, PlayError> {
        let target_secs = seconds.max(0.0);
        let target = Duration::from_secs_f64(target_secs);

        let Some(slot_id) = self.slot() else {
            *self.core.start_position.lock() = Some(target);
            debug!(target_secs, "seek held until a track is loaded");
            return Ok(SeekOutcome::Landed {
                target,
                landed_at: target,
            });
        };

        let Some(playback) = self.core.engine.slot_playback(slot_id) else {
            return Err(PlayError::SlotNotFound(slot_id));
        };
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

        let seek_epoch = playback.next_seek_epoch();

        self.core.engine.begin_slot_seek(slot_id, target);

        if let Err(err) = self.send_to_slot(PlayerCmd::Seek {
            seek_epoch,
            seconds: target_secs,
        }) {
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
    pub fn select_item(&self, index: usize, playback: SelectionPlayback) -> Result<(), PlayError> {
        self.select_item_with_crossfade(
            index,
            SelectTransition {
                playback,
                crossfade: CrossfadeSettings {
                    duration: self.crossfade_duration(),
                    ..CrossfadeSettings::default()
                },
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
    ///
    /// Reselecting the already-current item is valid even though its resource was consumed by the
    /// load that made it current: the resource now lives in the processor as the playing track.
    pub fn select_item_with_crossfade(
        &self,
        index: usize,
        transition: SelectTransition,
    ) -> Result<(), PlayError> {
        let SelectTransition {
            playback,
            crossfade,
        } = transition;
        let crossfade = crossfade.validate()?;
        let items_len = self.item_count();
        if index >= items_len {
            return Err(PlayError::IndexOutOfRange {
                index,
                len: items_len,
            });
        }

        let reselecting_current =
            index == self.core.items.current_index() && self.core.items.is_announced(index);
        let has_resource = self.core.items.has_resource(index);

        let armed_for_index = self
            .phase
            .lock()
            .pending()
            .is_some_and(|p| !p.state.activated() && p.index == index);
        if !armed_for_index && !reselecting_current && !has_resource {
            return Err(PlayError::ItemConsumed { index });
        }

        self.ensure_engine_started()?;
        self.ensure_slot()?;

        let _ = self.send_to_slot(PlayerCmd::SetPrefetchDuration(self.prefetch_duration()));

        if armed_for_index {
            self.commit_next(index)?;
        } else if !reselecting_current {
            self.unarm_next_internal(Some(index));
            self.core.items.set_current(index);
            self.load_current_item_with(crossfade)?;
            self.announce_current_item(index);
        }

        self.apply_playback(playback);
        Ok(())
    }

    pub(crate) fn start_playback(&self, item_id: TrackId, duration_seconds: f64) {
        self.start_playback_with(
            item_id,
            duration_seconds,
            CrossfadeSettings {
                duration: self.crossfade_duration(),
                ..CrossfadeSettings::default()
            },
        );
    }

    /// Make `item_id` leading: the playhead reads describe it from here on, not only once the
    /// audio thread has taken it on.
    fn start_playback_with(
        &self,
        item_id: TrackId,
        duration_seconds: f64,
        settings: CrossfadeSettings,
    ) {
        let Some(playback) = self
            .slot()
            .and_then(|slot_id| self.core.engine.slot_playback(slot_id))
        else {
            return;
        };
        let epoch = playback.lead(duration_seconds);
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn {
            item_id,
            settings,
            epoch,
        }));
    }
}
