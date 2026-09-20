use std::ops::Deref;

use kithara_bufpool::HasPool;
use kithara_platform::{sync::Arc, time::Duration};

#[cfg(test)]
use super::super::core::PlayerImpl;
use super::super::{
    core::PlayerRuntime,
    state::{PendingNext, PendingNextState},
};
use crate::{
    api::{CrossfadeSettings, EngineEvent, TrackId},
    bridge::PlayerCmd,
    error::PlayError,
};

/// Outcome of resolving an arm request under the short phase lock, acted on
/// outside the lock to avoid holding it across `send_to_slot`.
enum ArmDecision {
    /// The same index is already armed; return its src verbatim.
    AlreadyArmed(Arc<str>),
    /// The slot was cleared; optionally unload the previous item.
    Clear(Option<TrackId>),
}

struct ActivatedPending {
    item_id: TrackId,
    duration_seconds: f64,
}

struct Handover<'a, S> {
    player: &'a PlayerRuntime<S>,
}

impl<'a, S> Handover<'a, S> {
    const fn new(player: &'a PlayerRuntime<S>) -> Self {
        Self { player }
    }
}

impl<S> Deref for Handover<'_, S> {
    type Target = PlayerRuntime<S>;

    fn deref(&self) -> &Self::Target {
        self.player
    }
}

impl<S> Handover<'_, S>
where
    S: HasPool<f32>,
{
    /// Mark the armed-next slot at `index` activated under a short lock,
    /// returning its src. `Ok(None)` when it was already activated.
    fn activate_pending(&self, index: usize) -> Result<Option<ActivatedPending>, PlayError> {
        let mut phase = self.phase.lock();
        let pending = phase
            .pending_mut()
            .and_then(|slot| slot.as_mut())
            .ok_or(PlayError::NotReady)?;
        if pending.index != index {
            return Err(PlayError::ArmIndexMismatch {
                requested: index,
                armed: pending.index,
            });
        }
        let outcome = if pending.state.activated() {
            None
        } else {
            let item_id = pending.item_id;
            let duration_seconds = pending.duration_seconds;
            pending.state = PendingNextState::ActivatedReady;
            Some(ActivatedPending {
                item_id,
                duration_seconds,
            })
        };
        drop(phase);
        Ok(outcome)
    }

    fn restore_pending(&self, index: usize) {
        if let Some(pending) = self.phase.lock().pending_mut().and_then(Option::as_mut)
            && pending.index == index
        {
            pending.state = PendingNextState::Armed;
        }
    }

    /// Load `items[index]` into the audio-thread arena in `Preloading`
    /// state, ready for sample-accurate gapless stitch (cf=0) or parallel
    /// fade (cf>0).
    ///
    /// If a different next is already armed, it is unloaded first.
    /// Idempotent for the same index. Returns `Some(src)` on success;
    /// `None` if `items[index]` is empty (loader hasn't filled it yet) or
    /// `index` is out of range.
    fn arm_next(&self, index: usize) -> Result<Option<Arc<str>>, PlayError> {
        let current_index = self.current_index();
        if index >= self.item_count() {
            return Ok(None);
        }

        // WHY: Resolve any already-armed slot under a short phase lock: either the same index is already armed (return early), or it must be
        // cleared and possibly unloaded outside the lock.
        let mut phase = self.phase.lock();
        let existing = phase.pending_mut().and_then(|slot| slot.as_ref());
        let decision = match existing {
            Some(existing) if existing.index == index => {
                ArmDecision::AlreadyArmed(existing.src.clone())
            }
            Some(existing) => {
                let unload = (!(existing.state.activated() && existing.index == current_index))
                    .then_some(existing.item_id);
                if let Some(slot) = phase.pending_mut() {
                    *slot = None;
                }
                ArmDecision::Clear(unload)
            }
            None => ArmDecision::Clear(None),
        };
        drop(phase);

        let to_unload = match decision {
            ArmDecision::AlreadyArmed(src) => return Ok(Some(src)),
            ArmDecision::Clear(unload) => unload,
        };
        if let Some(item_id) = to_unload {
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack { item_id });
        }

        let Some((item_id, src, duration_seconds)) = self.enqueue_to_processor(index)? else {
            return Ok(None);
        };
        if let Some(pending_slot) = self.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                item_id,
                index,
                duration_seconds,
                src: src.clone(),
                state: PendingNextState::Armed,
            });
        }
        Ok(Some(src))
    }

    /// Snapshot of the armed-next index. `None` when no slot is armed
    /// (or after `commit_next` has consumed it for the current handover).
    #[must_use]
    fn armed_next(&self) -> Option<usize> {
        self.phase
            .lock()
            .pending()
            .filter(|pending| !pending.state.activated())
            .map(|pending| pending.index)
    }

    /// Commit the previously armed next track and start the cross-fade.
    ///
    /// Sends `FadeIn` to the audio thread for the armed slot, updates
    /// the playlist current index, and publishes `CurrentItemChanged`.
    ///
    /// # Errors
    /// - [`PlayError::NotReady`] if no slot is armed.
    /// - [`PlayError::ArmIndexMismatch`] if `index` does not match
    ///   [`Self::armed_next`].
    fn commit_next(&self, index: usize) -> Result<(), PlayError> {
        self.commit_next_with(
            index,
            CrossfadeSettings {
                duration: self.crossfade_duration(),
                ..CrossfadeSettings::default()
            },
        )
    }

    /// Commit the armed track with the profile captured by queue selection.
    fn commit_next_with(&self, index: usize, settings: CrossfadeSettings) -> Result<(), PlayError> {
        // WHY: `None` ⇒ the slot was already activated (idempotent no-op).
        let Some(activated) = self.activate_pending(index)? else {
            return Ok(());
        };

        if let Some(slot) = self.slot()
            && let Err(error) =
                self.core
                    .engine
                    .commit_track_transition(slot, activated.item_id, settings)
        {
            self.restore_pending(index);
            return Err(error);
        } else if self.slot().is_none() {
            self.start_playback_with(activated.item_id, settings);
        }
        self.publish_crossfade_started(settings);
        self.publish_current_track_snapshot(activated.duration_seconds);
        let current_index = self.current_index();
        if index != current_index {
            if let Some(outgoing) = self.core.items.current_item_id() {
                self.core.items.cancel_outgoing_free_adoption(outgoing);
            }
            self.core.items.set_current(index);
            self.announce_current_item(index);
        }
        Ok(())
    }

    fn publish_crossfade_started(&self, settings: CrossfadeSettings) {
        let Some(slot) = self.slot() else {
            return;
        };
        self.core
            .engine
            .bus()
            .publish(EngineEvent::CrossfadeStarted {
                from: slot,
                to: slot,
                duration: Duration::from_secs_f32(settings.duration.max(0.0)),
            });
    }

    /// Drop the armed next slot without committing.
    ///
    /// Sends `UnloadTrack` to the audio thread for the armed item and
    /// clears the pending slot. Skips the unload if the armed slot has
    /// already been activated for the current index (the activated track
    /// is now the leading one — unloading would silence playback).
    fn unarm_next(&self) {
        self.unarm_next_internal(Some(self.current_index()));
    }

    fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        let pending = self.phase.lock().pending_mut().and_then(Option::take);
        let Some(pending) = pending else {
            return;
        };
        let preserve_active_current = current_index_hint
            .is_some_and(|index| pending.state.activated() && pending.index == index);
        if !preserve_active_current {
            if pending.state.activated() {
                self.core
                    .engine
                    .bus()
                    .publish(EngineEvent::CrossfadeCancelled);
            }
            let _ = self.send_to_slot(PlayerCmd::UnloadTrack {
                item_id: pending.item_id,
            });
        }
    }
}

impl<S> PlayerRuntime<S>
where
    S: HasPool<f32>,
{
    pub fn arm_next(&self, index: usize) -> Result<Option<Arc<str>>, PlayError> {
        Handover::new(self).arm_next(index)
    }

    #[must_use]
    pub fn armed_next(&self) -> Option<usize> {
        Handover::new(self).armed_next()
    }

    pub fn commit_next(&self, index: usize) -> Result<(), PlayError> {
        Handover::new(self).commit_next(index)
    }

    pub(crate) fn commit_next_with(
        &self,
        index: usize,
        settings: CrossfadeSettings,
    ) -> Result<(), PlayError> {
        Handover::new(self).commit_next_with(index, settings)
    }

    pub fn unarm_next(&self) {
        Handover::new(self).unarm_next();
    }

    pub(crate) fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        Handover::new(self).unarm_next_internal(current_index_hint);
    }
}

#[cfg(test)]
mod tests {
    #[derive(Clone, Debug, kithara_events::EventSet)]
    enum TestEvent {
        Engine(EngineEvent),
        Player(PlayerEvent),
    }

    use kithara_events::Envelope;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig,
        api::{EngineEvent, PlayerEvent},
        mock,
        player::PlayerConfig,
        test_pools::{TestPools, pools},
    };

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    #[kithara::test]
    fn commit_next_without_arm_returns_not_ready() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        let err = player.commit_next(1).expect_err("must error");
        assert!(matches!(err, PlayError::NotReady));
    }

    #[kithara::test]
    fn failed_commit_restores_the_armed_selection() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        player
            .ensure_engine_started()
            .expect("engine start must succeed");
        player.ensure_slot().expect("slot allocation must succeed");
        if let Some(pending_slot) = player.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                item_id: TrackId::allocate(),
                src: Arc::from("next.mp3"),
                state: PendingNextState::Armed,
                index: 1,
                duration_seconds: 162.0,
            });
        }
        while player
            .send_to_slot(PlayerCmd::SetPaused {
                paused: true,
                item_id: None,
            })
            .is_ok()
        {}

        assert!(matches!(
            player.commit_next(1),
            Err(PlayError::SlotChannelFull { .. })
        ));
        assert_eq!(player.armed_next(), Some(1));
    }

    #[kithara::test]
    fn commit_next_publishes_snapshot_before_current_item_changed() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        player
            .ensure_engine_started()
            .expect("engine start must succeed");
        player.ensure_slot().expect("slot allocation must succeed");
        let mut rx = player.subscribe();

        if let Some(pending_slot) = player.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                item_id: TrackId::allocate(),
                src: Arc::from("next.mp3"),
                state: PendingNextState::Armed,
                index: 1,
                duration_seconds: 162.0,
            });
        }

        let settings = CrossfadeSettings {
            duration: 0.25,
            curve: crate::CrossfadeCurve::Linear,
            depth: 0.75,
            position: 0.25,
        };
        player
            .commit_next_with(1, settings)
            .expect("commit_next must succeed");

        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: TestEvent::Engine(EngineEvent::CrossfadeStarted { duration, .. }),
                ..
            }) if duration == Duration::from_secs_f32(settings.duration)
        ));
        assert_eq!(player.duration_seconds(), Some(162.0));
        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: TestEvent::Player(PlayerEvent::CurrentItemChanged { .. }),
                ..
            })
        ));
    }
}
