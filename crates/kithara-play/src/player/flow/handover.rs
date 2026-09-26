use std::ops::Deref;

use kithara_bufpool::HasPool;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_sync::LoadGeneration;

#[cfg(test)]
use super::super::PlayerImpl;
use super::super::{
    core::PlayerRuntime,
    state::{PendingNext, PendingNextState},
};
use crate::{
    api::{EngineEvent, TrackId},
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
    load: LoadGeneration,
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
            let load = pending.load;
            let duration_seconds = pending.duration_seconds;
            pending.state = PendingNextState::ActivatedReady;
            Some(ActivatedPending {
                item_id,
                load,
                duration_seconds,
            })
        };
        drop(phase);
        Ok(outcome)
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

        let Some((item_id, load, src, duration_seconds)) =
            self.enqueue_to_processor(index, None)?
        else {
            return Ok(None);
        };
        if let Some(pending_slot) = self.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                item_id,
                load,
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
        let Some(activated) = self.activate_pending(index)? else {
            return Ok(());
        };

        if let Err(error) = self.start_playback(activated.item_id) {
            if let Some(pending) = self
                .phase
                .lock()
                .pending_mut()
                .and_then(Option::as_mut)
                .filter(|pending| {
                    pending.item_id == activated.item_id && pending.load == activated.load
                })
            {
                pending.state = PendingNextState::Armed;
            }
            return Err(error);
        }
        self.phase
            .lock()
            .set_resident((activated.item_id, activated.load));
        self.publish_crossfade_started();
        self.publish_current_track_snapshot(activated.duration_seconds);
        let current_index = self.current_index();
        if index != current_index {
            self.core.items.set_current(index);
            self.announce_current_item(index);
        }
        Ok(())
    }

    fn publish_crossfade_started(&self) {
        let Some(slot) = self.slot() else {
            return;
        };
        self.core
            .engine
            .bus()
            .publish(EngineEvent::CrossfadeStarted {
                from: slot,
                to: slot,
                duration: Duration::from_secs_f32(self.crossfade_duration().max(0.0)),
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

    pub fn unarm_next(&self) {
        Handover::new(self).unarm_next();
    }

    pub(crate) fn unarm_next_internal(&self, current_index_hint: Option<usize>) {
        Handover::new(self).unarm_next_internal(current_index_hint);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroU32,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[derive(Clone, Debug, kithara_events::EventSet)]
    enum TestEvent {
        Engine(EngineEvent),
        Player(PlayerEvent),
    }

    use kithara_audio::{
        SeekBegin, SeekOutcome,
        mock::{AudioControlMock, AudioReadMock, AudioSessionMock},
    };
    use kithara_events::{Envelope, EventBus};
    use kithara_platform::time::Duration;
    use kithara_signal::AudioSpec;
    use kithara_test_utils::kithara;
    use kithara_warp::BeatGrid;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        PlayWorker, PlayWorkerConfig,
        api::{CrossfadeSettings, EngineEvent, PlayerEvent, SelectionPlayback},
        mock,
        player::{PlayerConfig, flow::SelectTransition},
        resource::Resource,
        test_pools::{TestPools, pools},
    };

    fn worker() -> PlayWorker<TestPools> {
        PlayWorker::new(PlayWorkerConfig::builder(pools()).build())
    }

    fn resource(src: &str) -> Resource {
        resource_with_seek(src, None)
    }

    fn resource_with_seek(src: &str, seek: Option<Arc<dyn SeekBegin>>) -> Resource {
        let reader = Unimock::new((
            AudioSessionMock::event_bus
                .each_call(matching!())
                .answers(&|mock| mock.make_ref(EventBus::new(1))),
            AudioSessionMock::duration
                .each_call(matching!())
                .returns(Some(Duration::from_secs(1))),
            AudioReadMock::spec
                .each_call(matching!())
                .returns(AudioSpec::new(
                    2,
                    NonZeroU32::new(44_100).expect("fixture rate"),
                )),
            AudioControlMock::preload
                .next_call(matching!())
                .returns(Ok(())),
            AudioControlMock::seek_handle
                .each_call(matching!())
                .returns(seek),
        ));
        Resource::from_reader(reader, Some(Arc::from(src)))
    }

    struct SeekCounter(Arc<AtomicU64>);

    impl SeekBegin for SeekCounter {
        fn begin(&self, position: Duration) -> SeekOutcome {
            self.0.fetch_add(1, Ordering::Relaxed);
            SeekOutcome::Landed {
                target: position,
                landed_at: position,
            }
        }
    }

    #[kithara::test]
    fn preloading_successor_keeps_committed_load_until_handover() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        let first_id = TrackId::allocate();
        let second_id = TrackId::allocate();
        player.insert(resource("first"), first_id, None);
        player.insert(resource("second"), second_id, None);
        player.play();
        let control = player.make_control();
        let first = control
            .resident_sync_observation()
            .expect("player open")
            .expect("first load committed");
        assert_eq!(first.item_id(), first_id);
        assert_eq!(first.load(), LoadGeneration::first());

        player.arm_next(1).expect("preload accepted");
        let still_first = control
            .resident_sync_observation()
            .expect("player open")
            .expect("first load still committed");
        assert_eq!(
            (still_first.item_id(), still_first.load()),
            (first_id, first.load())
        );

        player.commit_next(1).expect("handover accepted");
        let second = control
            .resident_sync_observation()
            .expect("player open")
            .expect("second load committed");
        assert_eq!(second.item_id(), second_id);
        assert_eq!(
            second.load(),
            first.load().checked_next().expect("fixture generation")
        );
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

        let item_id = TrackId::allocate();
        let load = LoadGeneration::first();
        if let Some(pending_slot) = player.phase.lock().pending_mut() {
            *pending_slot = Some(PendingNext {
                item_id,
                load,
                src: Arc::from("next.mp3"),
                state: PendingNextState::Armed,
                index: 1,
                duration_seconds: 162.0,
            });
        }

        let control = player.make_control();
        assert!(
            control
                .resident_sync_observation()
                .expect("player open")
                .is_none(),
            "preloaded successor is not yet the committed resident"
        );

        player.commit_next(1).expect("commit_next must succeed");

        player.pause();
        let observation = control
            .resident_sync_observation()
            .expect("player open")
            .expect("committed resident");
        assert_eq!((observation.item_id(), observation.load()), (item_id, load));
        assert!(matches!(
            observation.render(),
            crate::ResidentRender::Missing
        ));
        assert_eq!(observation.staging(), crate::ResidentStaging::Unavailable);

        assert!(matches!(
            rx.try_recv(),
            Ok(Envelope {
                event: TestEvent::Engine(EngineEvent::CrossfadeStarted { .. }),
                ..
            })
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

    #[kithara::test]
    fn rejected_load_does_not_advance_generation_or_publish_resident() {
        let (session, mock_session) = mock::session_with_drain();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(session)
                .build(),
        );
        player.ensure_engine_started().expect("engine started");
        player.ensure_slot().expect("slot allocated");
        let before_grid = player.core.track_grid.snapshot().revision();
        player.insert(resource("rejected"), TrackId::allocate(), None);
        for _ in 0..32 {
            player
                .send_to_slot(PlayerCmd::SetPaused(true))
                .expect("fixture fills command ring");
        }

        assert!(matches!(
            player.enqueue_to_processor(0, None),
            Err(PlayError::SlotChannelFull { .. })
        ));
        assert_eq!(*player.core.last_load.lock(), None);
        assert!(player.core.items.has_resource(0));
        assert_eq!(player.core.track_grid.snapshot().revision(), before_grid);
        assert!(player.core.staging.stageable_media().is_none());
        assert!(
            player
                .make_control()
                .resident_sync_observation()
                .expect("player open")
                .is_none()
        );

        mock_session.drain_commands();
        let retry = player
            .enqueue_to_processor(0, None)
            .expect("capacity returned")
            .expect("resource was retained");
        assert_eq!(retry.1, LoadGeneration::first());
        assert_eq!(*player.core.last_load.lock(), Some(retry.1));
        assert!(!player.core.items.has_resource(0));
    }

    #[kithara::test]
    fn reserved_load_keeps_slot_alive_across_stop_and_close() {
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        player.ensure_engine_started().expect("engine started");
        let slot = player.ensure_slot().expect("slot allocated");
        let load = player
            .core
            .engine
            .reserve_slot_load(slot, None)
            .expect("load capacity reserved");

        assert!(matches!(
            player.core.engine.stop(),
            Err(PlayError::SlotBusy { slot: busy }) if busy == slot
        ));
        assert!(matches!(
            player.core.engine.close(),
            Err(PlayError::SlotBusy { slot: busy }) if busy == slot
        ));
        assert!(player.core.engine.is_running());
        assert_eq!(player.core.engine.active_slots(), vec![slot]);

        drop(load);
        player
            .core
            .engine
            .close()
            .expect("close after load release");
        assert!(!player.core.engine.is_running());
        assert!(player.core.engine.active_slots().is_empty());
    }

    #[kithara::test]
    fn resumed_loaded_deck_does_not_reserve_another_load() {
        let (session, mock_session) = mock::session_with_drain();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(session)
                .build(),
        );
        let item_id = TrackId::allocate();
        player.insert(resource("resident"), item_id, None);
        player.play();
        player.pause();
        let before = player
            .make_control()
            .resident_sync_observation()
            .expect("player open")
            .expect("track resident");
        mock_session.drain_commands();
        for _ in 0..29 {
            player
                .send_to_slot(PlayerCmd::SetPaused(true))
                .expect("fixture leaves three command entries");
        }

        player.play();

        let after = player
            .make_control()
            .resident_sync_observation()
            .expect("player open")
            .expect("same track resident");
        assert_eq!(
            (after.item_id(), after.load()),
            (before.item_id(), before.load())
        );
        assert_eq!(*player.core.last_load.lock(), Some(before.load()));
        assert!(mock_session.drain_pause_commands().ends_with(&[false]));
    }

    #[kithara::test]
    fn rejected_fade_in_does_not_publish_a_resident_or_track_snapshot() {
        let (session, mock_session) = mock::session_with_drain();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(session)
                .build(),
        );
        player.ensure_engine_started().expect("engine started");
        player.ensure_slot().expect("slot allocated");
        let item_id = TrackId::allocate();
        player.insert(resource("not-yet-activated"), item_id, None);
        let before_status = player.status();
        for _ in 0..29 {
            player
                .send_to_slot(PlayerCmd::SetPaused(true))
                .expect("fixture leaves room for load and two settings");
        }

        player.play();

        assert_eq!(player.status(), before_status);
        assert_eq!(player.duration_seconds(), None);
        assert_eq!(*player.core.last_load.lock(), None);
        assert!(player.core.items.has_resource(0));
        assert!(
            player
                .make_control()
                .resident_sync_observation()
                .expect("player open")
                .is_none()
        );

        mock_session.drain_commands();
        player.play();
        let resident = player
            .make_control()
            .resident_sync_observation()
            .expect("player open")
            .expect("retry committed the load and FadeIn");
        assert_eq!(
            (resident.item_id(), resident.load()),
            (item_id, LoadGeneration::first())
        );
        assert_eq!(player.duration_seconds(), Some(1.0));
    }

    #[kithara::test]
    fn rejected_select_retains_cursor_resident_and_resource_for_retry() {
        let (session, mock_session) = mock::session_with_drain();
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(session)
                .build(),
        );
        let first_id = TrackId::allocate();
        let second_id = TrackId::allocate();
        player.insert(resource("first"), first_id, None);
        player.insert(resource("second"), second_id, None);
        player.play();
        let control = player.make_control();
        let first = control
            .resident_sync_observation()
            .expect("player open")
            .expect("first resident");
        mock_session.drain_commands();
        for _ in 0..30 {
            player
                .send_to_slot(PlayerCmd::SetPaused(true))
                .expect("fixture leaves one entry after select setting");
        }
        let transition = SelectTransition {
            playback: SelectionPlayback::Play,
            crossfade: CrossfadeSettings::default(),
        };

        assert!(matches!(
            player.select_item_with_crossfade(1, transition),
            Err(PlayError::SlotChannelFull { .. })
        ));
        assert_eq!(player.current_index(), 0);
        assert!(player.core.items.has_resource(1));
        let after_rejection = control
            .resident_sync_observation()
            .expect("player open")
            .expect("old resident remains");
        assert_eq!(
            (after_rejection.item_id(), after_rejection.load()),
            (first.item_id(), first.load())
        );

        mock_session.drain_commands();
        player
            .select_item_with_crossfade(1, transition)
            .expect("retry commits load and FadeIn");
        assert_eq!(player.current_index(), 1);
        assert!(!player.core.items.has_resource(1));
        let after_retry = control
            .resident_sync_observation()
            .expect("player open")
            .expect("new resident committed");
        assert_eq!(after_retry.item_id(), second_id);
        assert_eq!(
            after_retry.load(),
            first.load().checked_next().expect("fixture generation")
        );
    }

    #[kithara::test]
    fn full_seek_ring_does_not_begin_reader_seek_or_publish_epoch() {
        let begins = Arc::new(AtomicU64::new(0));
        let player = PlayerImpl::new(
            PlayerConfig::builder()
                .sample_rate(mock::SAMPLE_RATE)
                .worker(worker())
                .session(mock::session())
                .build(),
        );
        player.ensure_engine_started().expect("engine started");
        player.ensure_slot().expect("slot allocated");
        let reader_seek: Arc<dyn SeekBegin> = Arc::new(SeekCounter(Arc::clone(&begins)));
        player.insert(
            resource_with_seek("seekable", Some(reader_seek)),
            TrackId::allocate(),
            None,
        );
        player.enqueue_to_processor(0, None).expect("load accepted");
        for _ in 0..31 {
            player
                .send_to_slot(PlayerCmd::SetPaused(true))
                .expect("fixture fills command ring after load");
        }
        let playback = player
            .core
            .engine
            .slot_playback(player.slot().expect("fixture slot"))
            .expect("fixture playback");
        let before_position = playback.position.load(Ordering::Relaxed);

        assert!(matches!(
            player.seek_seconds(0.25),
            Err(PlayError::SlotChannelFull { .. })
        ));
        assert_eq!(begins.load(Ordering::Relaxed), 0);
        assert_eq!(playback.seek_epoch.load(Ordering::SeqCst), 0);
        assert_eq!(playback.position.load(Ordering::Relaxed), before_position);
    }
}
