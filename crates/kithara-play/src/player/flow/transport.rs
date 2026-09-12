use std::sync::atomic::Ordering;

use kithara_audio::SeekOutcome;
use kithara_bufpool::HasPool;
use kithara_platform::time::Duration;
use kithara_warp::AssetFrame;
use tracing::{debug, warn};

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
            if !self.arm_prepared_launch_or_hold_source_cue() {
                let _ = self.send_to_slot(PlayerCmd::SetPaused {
                    paused: false,
                    item_id: self.core.items.current_item_id(),
                });
            }
            self.enter_playing();
            self.set_status(PlayerStatus::ReadyToPlay);
        } else {
            if let Some(slot) = self.slot() {
                self.core.engine.disarm_prepared_launches(slot);
            }
            let _ = self.send_to_slot(PlayerCmd::SetPaused {
                paused: true,
                item_id: None,
            });
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
        self.publish_current_track_snapshot(duration_seconds);
        self.start_playback(item_id);
        self.apply_start_position();
        Ok(true)
    }

    /// Pause playback. The effective rate becomes `0.0` when RT applies the command.
    pub fn pause(&self) {
        if let Some(slot) = self.slot() {
            self.core.engine.disarm_prepared_launches(slot);
        }
        let _ = self.send_to_slot(PlayerCmd::SetPaused {
            paused: true,
            item_id: self.core.items.current_item_id(),
        });
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
        if !self.arm_prepared_launch_or_hold_source_cue() {
            let _ = self.send_to_slot(PlayerCmd::SetPaused {
                paused: false,
                item_id: self.core.items.current_item_id(),
            });
        }

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

    /// Arm a scheduled launch when present. If a synchronized source cue is
    /// awaiting its grid, retain the requested playing phase without releasing
    /// ordinary PCM.
    fn arm_prepared_launch_or_hold_source_cue(&self) -> bool {
        let Some(item) = self.core.items.current_item_id() else {
            return false;
        };
        let prepared = self
            .slot()
            .is_some_and(|slot| self.core.engine.set_prepared_launch_armed(slot, item, true));
        if prepared {
            self.core.items.consume_awaiting_initial_source_cue(item);
            true
        } else {
            self.core.items.hold_or_consume_initial_source_cue(item)
        }
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

        if let Some(item) = self.core.items.current_item_id() {
            self.core.items.clear_initial_source_cue(item);
        }

        let Some(slot_id) = self.slot() else {
            // No slot means no processor to carry the re-base, and refusing
            // here drops a real target: a host restores its stored position
            // while seeding the queue. Keep it — the load that starts the
            // current item places the track there instead of at its head.
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
        self.select_item_with_crossfade_from_source_cue(index, transition, None)
    }

    pub(crate) fn select_item_with_crossfade_from_source_cue(
        &self,
        index: usize,
        transition: SelectTransition,
        initial_source_cue: Option<AssetFrame>,
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

        if let Some(item) = self.core.items.current_item_id() {
            self.core
                .items
                .set_initial_source_cue(item, initial_source_cue);
        }

        self.apply_autoplay(autoplay);
        Ok(())
    }

    pub(crate) fn start_playback(&self, item_id: TrackId) {
        let _ = self.send_to_slot(PlayerCmd::Transition(TrackTransition::FadeIn(item_id)));
    }
}
