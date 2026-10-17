use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_events::TrackId;
use kithara_play::{
    Outbox, OutputSnapshot, PlayError, Player, PlayerConfig, Position, Slot, Track, TrackCommand,
    TrackFactory, TrackStatus as PlayingStatus,
};
use tracing::warn;

use super::{
    Queue, Transition,
    slots::{Active, LoadState, Parked, Role},
    transition::TransitionRequest,
    types::{Continuation, PendingHandover, Placement, SeekPosition, extract_track_name},
};
use crate::{
    AdvanceReason, NavigationState, QueueError, QueueEvent, TrackSource, TrackStatus, consts,
    track::TrackRecord,
};

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) fn insert_entry(
        &mut self,
        id: TrackId,
        source: TrackSource<S>,
        position: Position,
        placement: Placement,
    ) {
        let record = TrackRecord::new(id, extract_track_name(&source), source, position);
        let records = self.tracks.records_mut();
        let index = match placement {
            Placement::Append => {
                records.push(record);
                records.len() - 1
            }
            Placement::At(index) => {
                records.insert(index, record);
                index
            }
        };
        self.navigation.insert(id, self.current);
        self.announce(QueueEvent::TrackAdded { id, index });
    }

    pub(super) fn arm_initial_load(
        &mut self,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) {
        if self.clock.is_none()
            || output.is_none()
            || self.current.is_some()
            || self.target.is_some()
            || self
                .active
                .iter()
                .any(|active| active.role == Role::Current)
        {
            return;
        }
        if let Err(error) = self.next_target(
            Transition::None,
            AdvanceReason::InitialLoad,
            false,
            self.config.should_autoplay,
            output,
            out,
        ) {
            warn!(%error, "the initial load could not start");
        }
    }

    pub(super) fn remove_entry(
        &mut self,
        id: TrackId,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let index = self
            .tracks
            .records()
            .iter()
            .position(|record| record.id == id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let current = self.current == Some(id);
        let pending = self
            .pending_handover
            .filter(|pending| pending.item == id && !pending.replacement);
        let replacement = if current || pending.is_some() {
            self.tracks
                .records()
                .get(index + 1)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|previous| self.tracks.records().get(previous))
                })
                .map(|record| record.id)
        } else {
            None
        };
        let shifted = self.current_index().is_some_and(|current| current > index);
        if self.target.is_some_and(|target| target.to == id) {
            self.cancel_target(out)?;
        }
        let (sent, removal_seq) = self.release_removed_item(id, replacement, pending, out)?;
        drop(self.tracks.records_mut().remove(index));
        self.navigation.reconcile(&self.track_ids());
        self.announce(QueueEvent::TrackRemoved { id });
        if shifted {
            self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
        }
        if let Some(replacement) = replacement.filter(|_| current || removal_seq.is_some()) {
            let entry = self.request_transition(
                TransitionRequest {
                    id: replacement,
                    transition: Transition::None,
                    reason: AdvanceReason::RemovedCurrent,
                    auto: false,
                    playing: true,
                },
                output,
                out,
            )?;
            if let Some(pending) = pending
                && let Some(seq) = removal_seq
                && let Some(target) = &mut self.target
            {
                target.continued = Some(Continuation::Removal {
                    seq,
                    handover: pending.seq,
                    slot: pending.slot,
                    applied: false,
                });
            }
            return Ok(sent.or(entry));
        }
        self.reap_released();
        Ok(sent)
    }

    fn release_removed_item(
        &mut self,
        id: TrackId,
        replacement: Option<TrackId>,
        pending: Option<PendingHandover>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(Option<Seq>, Option<Seq>), PlayError> {
        let mut sent = None;
        let mut removal_seq = None;
        for active_index in self.active.indices(|active| {
            active.item == id && !(active.role == Role::Current && replacement.is_some())
        }) {
            let pending_owner = pending.is_some_and(|pending| {
                self.active
                    .get(active_index)
                    .is_some_and(|active| active.slot == pending.slot)
                    && self.active.is_replacement(active_index) == pending.replacement
            });
            let released = self.release_track(active_index, out)?;
            sent = released.or(sent);
            if pending_owner {
                removal_seq = released;
            }
        }
        for parked in self
            .active
            .parked_iter_mut()
            .filter(|parked| parked.item == id)
        {
            sent = parked.track.apply(TrackCommand::Release, out)?.or(sent);
        }
        Ok((sent, removal_seq))
    }

    pub(super) fn clear_entries(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let sent = self.release_all(out)?;
        self.target = None;
        self.current = None;
        self.held_position = None;
        let ids = self.track_ids();
        self.tracks.records_mut().clear();
        let repeat = self.navigation.repeat_mode();
        let order = self.navigation.playback_order();
        self.navigation = NavigationState::new(self.navigation.history_limit());
        self.navigation.set_repeat(repeat);
        self.navigation.set_playback_order(order, &[], None);
        for id in ids {
            self.announce(QueueEvent::TrackRemoved { id });
        }
        self.reap_released();
        Ok(sent)
    }

    pub(super) fn load_track(
        &mut self,
        id: TrackId,
        role: Role,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let reusable_current = self.pending_handover.is_some()
            && self.current == Some(id)
            && self
                .target
                .is_some_and(|target| !target.auto && target.to == id);
        if let Some(index) = self.active.position(|active| {
            active.item == id
                && (matches!(active.role, Role::Incoming | Role::Preloaded)
                    || (reusable_current
                        && active.role == Role::Current
                        && matches!(
                            active.track.snapshot().as_ref().status,
                            PlayingStatus::Loaded
                                | PlayingStatus::Paused { .. }
                                | PlayingStatus::Playing { .. }
                                | PlayingStatus::Faded { .. }
                        )))
        }) {
            let active = self.active.get_mut(index).ok_or(QueueError::NotReady(id))?;
            if active.role != Role::Current {
                active.role = role;
            }
            return Ok(active.load.map(LoadState::seq));
        }
        if self
            .tracks
            .records()
            .iter()
            .any(|record| record.id == id && record.status == TrackStatus::Consumed)
        {
            self.tracks.set_status(id, TrackStatus::Pending);
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck").into());
        }
        let taken = self
            .active
            .parked_position(|parked| {
                parked.item == id
                    && parked.track.snapshot().as_ref().status != PlayingStatus::Released
            })
            .map(|index| self.active.take_parked(index));
        let Some(slot) = self.active.free_slot() else {
            return self.evict_for(id, role, taken, output, out);
        };
        let (track, load) = if let Some(mut parked) = taken {
            let result = (|| {
                if let Some(to) = self.held_position.and_then(SeekPosition::held) {
                    parked.track.apply(TrackCommand::Seek { to }, out)?;
                    self.held_position = None;
                }
                parked.track.apply(
                    TrackCommand::Seat {
                        slot,
                        at: When::Next,
                    },
                    out,
                )
            })();
            if let Err(error) = result {
                if let Err(error) = parked.track.apply(TrackCommand::Release, out) {
                    warn!(%error, "a refused background seat waits to release");
                }
                self.active.park(parked);
                return Err(error.into());
            }
            (parked.track, parked.load)
        } else {
            let (track, seq) = self.open_track(
                id,
                Some(slot),
                self.held_position.and_then(SeekPosition::held),
                output,
                out,
            )?;
            if matches!(self.held_position, Some(SeekPosition::Held(_))) {
                self.held_position = None;
            }
            (track, seq.map(LoadState::Opening))
        };
        let seq = load.map(LoadState::seq);
        self.active.push(Active {
            item: id,
            slot,
            track,
            role,
            load,
        });
        Ok(seq)
    }

    fn open_track(
        &mut self,
        id: TrackId,
        slot: Option<Slot>,
        held_position: Option<Position>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(F::Track, Option<Seq>), QueueError> {
        if out.dispatcher_available() == 0 {
            return Err(PlayError::Full("dispatcher").into());
        }
        let source = self
            .tracks
            .source(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let position = held_position.unwrap_or(
            self.tracks
                .initial_position(id)
                .ok_or(QueueError::UnknownTrackId(id))?,
        );
        let observer = self
            .tracks
            .observer(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let settings = self
            .current_track()
            .map_or(self.config.track, Track::projected);
        let mut track = self.config.factory.track(PlayerConfig {
            item: id,
            slot,
            settings,
        })?;
        let loader = self
            .loader
            .as_ref()
            .ok_or_else(|| PlayError::InvalidConfiguration {
                reason: "a hosted queue requires resource preparation".into(),
            })?;
        let output = output.ok_or(PlayError::NotReady)?;
        let (item, load) = loader.start(id, source, observer, output)?;
        let seq = track.apply(TrackCommand::Load { item, position }, out)?;
        self.tracks.begin_load(id, load);
        Ok((track, seq))
    }

    fn evict_for(
        &mut self,
        id: TrackId,
        role: Role,
        taken: Option<Parked<F::Track>>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let staging = (|| {
            if let Some(index) = self.active.replacement_index() {
                self.release_track(index, out)?;
                self.reap_released();
            }
            let victim = self
                .active
                .quietest(&self.deck.mixer, |_| true)
                .ok_or_else(|| PlayError::InvalidConfiguration {
                    reason: "a mixer must have at least one slot".into(),
                })?;
            Ok::<_, QueueError>(
                self.active
                    .get(victim)
                    .ok_or(QueueError::NotReady(id))?
                    .slot,
            )
        })();
        let slot = match staging {
            Ok(slot) => slot,
            Err(error) => {
                if let Some(parked) = taken {
                    self.active.park(parked);
                }
                return Err(error);
            }
        };
        let (track, load) = if let Some(mut parked) = taken {
            if let Some(to) = self.held_position.and_then(SeekPosition::held) {
                if let Err(error) = parked.track.apply(TrackCommand::Seek { to }, out) {
                    if let Err(error) = parked.track.apply(TrackCommand::Release, out) {
                        warn!(%error, "a refused background seek waits to release");
                    }
                    self.active.park(parked);
                    return Err(error.into());
                }
                self.held_position = None;
            }
            (parked.track, parked.load)
        } else {
            let (track, seq) = self.open_track(
                id,
                None,
                self.held_position.and_then(SeekPosition::held),
                output,
                out,
            )?;
            if matches!(self.held_position, Some(SeekPosition::Held(_))) {
                self.held_position = None;
            }
            (track, seq.map(LoadState::Opening))
        };
        let seq = load.map(LoadState::seq);
        self.active.stage(Active {
            item: id,
            slot,
            track,
            role,
            load,
        });
        Ok(seq)
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) fn release_track(
        &mut self,
        index: usize,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let replacement = self.active.is_replacement(index);
        let active = self.active.get_mut(index).ok_or(PlayError::NoActiveSlot)?;
        let sent = active.track.apply(TrackCommand::Release, out)?;
        let slot = active.slot;
        active.role = Role::Leaving;
        if !replacement {
            self.active.clear_fade(slot);
        }
        Ok(sent)
    }

    pub(super) fn release_all(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let mut detach = false;
        let mut dispatcher = self.active.parked_len();
        for active in self.active.iter() {
            if active.track.snapshot().as_ref().attached {
                detach = true;
            } else {
                dispatcher += 1;
            }
        }
        if detach && out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        if out.dispatcher_available() < dispatcher {
            return Err(PlayError::Full("dispatcher"));
        }
        let mut release = |out: &mut Outbox<'_, S>| {
            for active in self.active.iter_mut() {
                active.track.apply(TrackCommand::Release, out)?;
            }
            for parked in self.active.parked_iter_mut() {
                parked.track.apply(TrackCommand::Release, out)?;
            }
            Ok::<(), PlayError>(())
        };
        let sent = if detach {
            out.together(When::Next, release)?.1
        } else {
            release(out)?;
            None
        };
        for active in self.active.iter_mut() {
            active.role = Role::Leaving;
        }
        Ok(sent)
    }

    pub(super) fn close_tracks(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let sent = self.release_all(out)?;
        self.target = None;
        self.shutdown.cancel();
        self.tracks.cancel_loads();
        self.reap_released();
        Ok(sent)
    }

    pub(super) fn reap_released(&mut self) {
        for index in (0..self.active.parked_len()).rev() {
            if self.active.parked_get_mut(index).is_some_and(|parked| {
                parked.track.snapshot().as_ref().status == PlayingStatus::Released
                    && !matches!(parked.load, Some(LoadState::Opening(_)))
            }) {
                self.active.take_parked(index);
            }
        }
        for index in self
            .active
            .indices(|active| {
                active.track.snapshot().as_ref().status == PlayingStatus::Released
                    && !matches!(active.load, Some(LoadState::Opening(_)))
                    && self
                        .pending_handover
                        .is_none_or(|pending| pending.item != active.item)
            })
            .into_iter()
            .rev()
        {
            let active = self.active.remove(index);
            if active.role == Role::Leaving
                && self.current == Some(active.item)
                && self.active_current_index().is_none()
            {
                self.current = None;
                self.announce(QueueEvent::CurrentTrackChanged { id: None });
            }
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog]
    pub(super) fn pump_loads(&mut self, output: Option<&OutputSnapshot>, out: &mut Outbox<'_, S>) {
        if self.shutdown.is_cancelled() || output.is_none() {
            return;
        }
        while self.active.parked_len() < self.config.max_concurrent_loads.get()
            && out.dispatcher_available() > consts::SELECT_DISPATCH_RESERVE
        {
            hang_tick!();
            let Some(id) = self
                .tracks
                .records()
                .iter()
                .find(|record| record.status == TrackStatus::Pending)
                .map(|record| record.id)
            else {
                break;
            };
            match self.open_track(id, None, None, output, out) {
                Ok((track, load)) => self.active.park(Parked {
                    item: id,
                    track,
                    load: load.map(LoadState::Opening),
                }),
                Err(QueueError::Play(PlayError::Full(_) | PlayError::Closed)) => break,
                Err(error) => {
                    self.tracks.fail(id, &error);
                    self.announce(QueueEvent::TrackLoadFailed {
                        id,
                        reason: error.to_string(),
                        auto_skipped: false,
                    });
                }
            }
            hang_reset!();
        }
    }

    pub(super) fn reopen_requested_load(
        &mut self,
        index: usize,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), QueueError> {
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let id = active.item;
        let position = active.track.snapshot().as_ref().position;
        let source = self
            .tracks
            .source(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let observer = self
            .tracks
            .observer(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let loader = self.loader.as_ref().ok_or(PlayError::NotReady)?;
        let output = output.ok_or(PlayError::NotReady)?;
        let (item, load) = loader.start(id, source, observer, output)?;
        let seq = self
            .active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(TrackCommand::Load { item, position }, out)?;
        self.active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .load = seq.map(LoadState::Opening);
        self.tracks.begin_load(id, load);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_host::{Host, HostConfig, HostOwned};
    use kithara_platform::{
        time::{self, Duration},
        tokio::task::spawn_blocking,
    };
    use kithara_play::{
        DeckEvent, HostedDeck, PlayWorker, PlayWorkerConfig, ResourcePrep, Slot, TrackReceipt,
    };
    use kithara_test_utils::kithara;
    #[cfg(not(target_arch = "wasm32"))]
    use kithara_test_utils::off_thread::OffThread;

    use super::*;
    use crate::{
        QueueConfig, QueueControl,
        event::QueueEvent,
        queue::{
            QueueCommand,
            state::tests::{apply, make_queue, wait_for_queue_event, with_outbox},
        },
        test_pools::{TestPools, pools},
    };

    #[cfg(not(target_arch = "wasm32"))]
    type QueueHost = OffThread<Host<TestPools>>;
    #[cfg(target_arch = "wasm32")]
    type QueueHost = Host<TestPools>;

    fn insert_queue(host: &mut Host<TestPools>) -> HostOwned<Queue<TestPools>> {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .build();
        let config = QueueConfig::builder()
            .prep(prep)
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build();
        host.insert(Queue::new(config))
            .expect("insert fixture queue")
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    #[kithara::flash(io)]
    async fn hosted_queue() -> (HostOwned<Queue<TestPools>>, QueueHost) {
        hang_tick!();
        let fixture = time::timeout(__hang_detector.remaining(), async {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let host = OffThread::spawn("queue-lifecycle-host", || {
                    Host::new(HostConfig::offline(pools()).build())
                })
                .await
                .expect("fixture offline Host");
                let queue = host.call(insert_queue).await;
                (queue, host)
            }
            #[cfg(target_arch = "wasm32")]
            {
                spawn_blocking(|| {
                    let mut host = Host::new(HostConfig::offline(pools()).build())
                        .expect("fixture offline Host");
                    let queue = insert_queue(&mut host);
                    (queue, host)
                })
                .await
                .expect("create queue on the blocking test worker")
            }
        })
        .await;
        hang_tick!();
        let fixture = fixture.expect("queue fixture initializes within its liveness budget");
        hang_reset!();
        fixture
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    #[kithara::flash(io)]
    async fn close_host(host: QueueHost) {
        hang_tick!();
        let closed = time::timeout(__hang_detector.remaining(), async move {
            #[cfg(not(target_arch = "wasm32"))]
            host.close().await;
            #[cfg(target_arch = "wasm32")]
            drop(host);
        })
        .await;
        hang_tick!();
        closed.expect("queue Host teardown completes within its liveness budget");
        hang_reset!();
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    #[kithara::flash(io)]
    async fn command<R: Send + 'static>(
        queue: &QueueControl<TestPools>,
        run: impl FnOnce(QueueControl<TestPools>) -> R + Send + 'static,
    ) -> R {
        let control = queue.clone();
        hang_tick!();
        let answer = time::timeout(
            __hang_detector.remaining(),
            spawn_blocking(move || run(control)),
        )
        .await;
        hang_tick!();
        let answer = answer
            .expect("queue command completes within its liveness budget")
            .expect("queue command worker must not panic");
        hang_reset!();
        answer
    }

    async fn append(queue: &QueueControl<TestPools>, source: &str) -> TrackId {
        let source = source.to_owned();
        command(queue, move |control| {
            control
                .append(source)
                .expect("BUG: open queue must accept a track")
        })
        .await
    }

    #[kithara::test(tokio)]
    async fn len_is_empty_reflect_append() {
        let (queue, host) = hosted_queue().await;
        assert!(queue.is_empty());
        let _ = append(&queue, "https://example.com/a.mp3").await;
        let _ = append(&queue, "https://example.com/b.mp3").await;
        assert_eq!(queue.len(), 2);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn append_returns_monotonic_ids_and_emits_track_added() {
        let (queue, host) = hosted_queue().await;
        let mut rx = queue.subscribe();
        let a = append(&queue, "https://example.com/a.mp3").await;
        let b = append(&queue, "https://example.com/b.mp3").await;
        assert_ne!(a, b);
        assert!(a.as_u64() < b.as_u64());

        let mut seen = 0;
        while wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackAdded { .. }),
            200,
        )
        .await
        {
            hang_tick!();
            seen += 1;
            hang_reset!();
            if seen == 2 {
                break;
            }
        }
        assert_eq!(seen, 2);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn remove_drops_from_queue_and_emits() {
        let (queue, host) = hosted_queue().await;
        let a = append(&queue, "https://example.com/a.mp3").await;
        let _b = append(&queue, "https://example.com/b.mp3").await;
        let mut rx = queue.subscribe();

        command(&queue, move |control| control.remove(a))
            .await
            .expect("BUG: just-appended track must be removable");
        assert_eq!(queue.len(), 1);
        let saw_removed = wait_for_queue_event(
            &mut rx,
            |ev| matches!(ev, QueueEvent::TrackRemoved { id } if id == &a),
            300,
        )
        .await;
        assert!(saw_removed);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn clear_empties_queue() {
        let (queue, host) = hosted_queue().await;
        let _a = append(&queue, "https://example.com/a.mp3").await;
        let _b = append(&queue, "https://example.com/b.mp3").await;
        assert_eq!(queue.len(), 2);
        command(&queue, |control| control.clear())
            .await
            .expect("the idle deck takes the clear");
        assert_eq!(queue.len(), 0);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn clear_discards_old_eof_before_reinsert() {
        let (mut queue, mut rig) = make_queue();
        let old = TrackId::allocate();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id: old,
                source: "https://example.com/old.mp3".into(),
            },
        )
        .expect("open queue accepts a track");
        queue.current = Some(old);
        let ended = DeckEvent::Ended {
            slot: Slot::new(0),
            at: kithara_signal::SessionFrame::new(0),
        };

        apply(&mut queue, &mut rig, QueueCommand::RemoveAll)
            .expect("the idle deck takes the clear");
        let replacement = TrackId::allocate();
        apply(
            &mut queue,
            &mut rig,
            QueueCommand::Append {
                position: Position::ZERO,
                id: replacement,
                source: "https://example.com/replacement.mp3".into(),
            },
        )
        .expect("open queue accepts a replacement track");
        queue.current = Some(replacement);
        with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            HostedDeck::settle(queue, TrackReceipt::Event(ended), pass, out);
            HostedDeck::tick(queue, pass, out);
        });

        assert_eq!(
            queue.control().current().map(|entry| entry.id),
            Some(replacement),
            "an EOF queued before clear must not end the replacement queue"
        );
    }

    #[kithara::test(tokio)]
    async fn set_tracks_replaces_queue() {
        let (queue, host) = hosted_queue().await;
        let _a = append(&queue, "https://example.com/a.mp3").await;
        command(&queue, |control| {
            control.set_tracks(
                [
                    "https://example.com/1.mp3",
                    "https://example.com/2.mp3",
                    "https://example.com/3.mp3",
                ]
                .map(TrackSource::from),
            )
        })
        .await
        .expect("the idle deck takes the clear");
        assert_eq!(queue.len(), 3);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn insert_after_id_places_next() {
        let (queue, host) = hosted_queue().await;
        let a = append(&queue, "https://example.com/a.mp3").await;
        let b = append(&queue, "https://example.com/b.mp3").await;
        let mid = command(&queue, move |control| {
            control.insert("https://example.com/mid.mp3", Some(a))
        })
        .await
        .expect("BUG: insert relative to existing track");
        let snapshot = queue.tracks();
        let ids: Vec<TrackId> = snapshot.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a, mid, b]);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn track_source_is_keyed_by_id_across_removal() {
        let (queue, host) = hosted_queue().await;
        let a = append(&queue, "https://example.com/a.mp3").await;
        let b = append(&queue, "https://example.com/b.mp3").await;

        assert_eq!(
            queue
                .track_source(a)
                .and_then(|s| s.uri().map(str::to_string)),
            Some("https://example.com/a.mp3".to_string()),
            "source resolves by identity"
        );

        // Removing an earlier track must not shift which source `b` resolves
        // to, and the removed id must no longer have a source.
        command(&queue, move |control| control.remove(a))
            .await
            .expect("BUG: remove existing track");
        assert!(
            queue.track_source(a).is_none(),
            "removed track has no source"
        );
        assert_eq!(
            queue
                .track_source(b)
                .and_then(|s| s.uri().map(str::to_string)),
            Some("https://example.com/b.mp3".to_string()),
            "surviving track still resolves to its own source by id"
        );
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    async fn begin_dedupes_live_attempt() {
        let (mut queue, mut rig) = make_queue();
        queue.insert_entry(
            TrackId(1),
            "/kithara/missing.wav".into(),
            Position::ZERO,
            Placement::Append,
        );
        let first = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.load_track(TrackId(1), Role::Preloaded, Some(pass.output), out)
        })
        .expect("first load")
        .expect("new load has a sequence");
        let second = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.load_track(TrackId(1), Role::Preloaded, Some(pass.output), out)
        })
        .expect("duplicate load reuses the open");
        assert_eq!(second, Some(first));
        assert_eq!(queue.active.iter().count(), 1);
        assert_eq!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .expect("one open")
                .seq(),
            first
        );
        assert!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .is_none(),
            "no duplicate open"
        );
    }

    #[kithara::test(tokio)]
    async fn selection_reflects_only_the_requested_live_attempt() {
        let (mut queue, mut rig) = make_queue();
        for id in [TrackId(1), TrackId(2)] {
            queue.insert_entry(
                id,
                "/kithara/missing.wav".into(),
                Position::ZERO,
                Placement::Append,
            );
        }
        assert!(!queue.target.is_some_and(|target| target.to == TrackId(1)));
        assert!(
            with_outbox(&mut queue, &mut rig, |queue, pass, out| {
                queue.load_track(TrackId(1), Role::Preloaded, Some(pass.output), out)
            })
            .expect("prefetch")
            .is_some()
        );
        assert!(
            apply(
                &mut queue,
                &mut rig,
                QueueCommand::Select {
                    id: TrackId(2),
                    transition: Transition::None,
                }
            )
            .expect("selected load")
            .is_some()
        );
        assert!(!queue.target.is_some_and(|target| target.to == TrackId(1)));
        assert!(queue.target.is_some_and(|target| target.to == TrackId(2)));
        assert!(
            queue
                .active
                .iter()
                .any(|active| active.item == TrackId(1) && active.role == Role::Preloaded)
        );
        assert!(
            queue
                .active
                .iter()
                .any(|active| active.item == TrackId(2) && active.role == Role::Incoming)
        );
    }

    #[kithara::test(tokio)]
    async fn promote_waiting_reuses_its_open_and_preserves_its_claim() {
        let (mut queue, mut rig) = make_queue();
        queue.insert_entry(
            TrackId(1),
            "/kithara/missing.wav".into(),
            Position::ZERO,
            Placement::Append,
        );
        let queued = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.load_track(TrackId(1), Role::Preloaded, Some(pass.output), out)
        })
        .expect("queued preload")
        .expect("open sequence");
        let token = queue.tracks.records()[0]
            .load
            .as_ref()
            .expect("queued load")
            .token()
            .clone();
        let parked = queue.tracks.records()[0]
            .load
            .as_ref()
            .expect("queued load")
            .watch()
            .clone();
        let promoted = apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id: TrackId(1),
                transition: Transition::None,
            },
        )
        .expect("select promotes the queued preload");
        assert!(
            !token.is_cancelled(),
            "promotion preserves the original load"
        );
        assert!(
            !parked.is_cancelled(),
            "the original attempt retains its claim"
        );
        let replacement = queue.tracks.records()[0]
            .load
            .as_ref()
            .expect("promoted load");
        assert!(
            !replacement.watch().is_cancelled(),
            "the promoted attempt owns its claim"
        );
        assert!(!replacement.token().is_cancelled());
        assert_eq!(promoted, Some(queued));
        assert!(queue.incoming_index(TrackId(1)).is_some());
        assert!(
            !queue
                .active
                .iter()
                .any(|active| active.role == Role::Preloaded)
        );
        assert_eq!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .expect("original open")
                .seq(),
            queued
        );
        assert!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .is_none(),
            "promotion must not open another source"
        );
    }

    #[kithara::test(tokio)]
    async fn promote_keeps_attempt_holding_permit() {
        let (mut queue, mut rig) = make_queue();
        queue.insert_entry(
            TrackId(1),
            "/kithara/missing.wav".into(),
            Position::ZERO,
            Placement::Append,
        );
        let first = with_outbox(&mut queue, &mut rig, |queue, pass, out| {
            queue.load_track(TrackId(1), Role::Preloaded, Some(pass.output), out)
        })
        .expect("preloaded open")
        .expect("open sequence");
        rig.opens.drain();
        let token = queue.tracks.records()[0]
            .load
            .as_ref()
            .expect("admitted load")
            .token()
            .clone();
        let promoted = apply(
            &mut queue,
            &mut rig,
            QueueCommand::Select {
                id: TrackId(1),
                transition: Transition::None,
            },
        )
        .expect("promote admitted open");
        assert_eq!(
            promoted,
            Some(first),
            "an attempt past the permit gate keeps its download"
        );
        assert!(!token.is_cancelled());
        assert_eq!(queue.active.iter().count(), 1);
        assert_eq!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .expect("one open")
                .seq(),
            first
        );
        assert!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .is_none()
        );
    }

    #[kithara::test]
    fn promote_vacant_is_noop() {
        let (mut queue, mut rig) = make_queue();
        queue.insert_entry(
            TrackId(1),
            "/kithara/missing.wav".into(),
            Position::ZERO,
            Placement::Append,
        );
        assert!(
            with_outbox(&mut queue, &mut rig, |queue, _, out| {
                queue.transition_loaded(out)
            })
            .expect("no transition to promote")
            .is_none()
        );
        assert!(queue.tracks.records()[0].load.is_none());
        assert_eq!(queue.active.iter().count(), 0);
        assert!(
            rig.open(Err(kithara_play::LoadRefusal::Cancelled))
                .is_none()
        );
    }

    #[kithara::test]
    fn config_failure_without_runtime_updates_tracks_synchronously() {
        use kithara_events::Envelope;

        let (mut queue, mut rig) = make_queue();
        let source: TrackSource<TestPools> = "not a url".into();
        for id in [TrackId(42), TrackId(43)] {
            queue.insert_entry(id, source.clone(), Position::ZERO, Placement::Append);
        }
        queue.publish();
        let output = kithara_play::mock::output(None).get();
        let Err(expected) =
            queue
                .loader
                .as_ref()
                .expect("loader")
                .build_config(TrackId(42), source, &output)
        else {
            panic!("fixture source must be rejected");
        };
        assert!(matches!(expected, QueueError::InvalidUrl(_)));
        let reason = expected.to_string();
        let mut events = queue.subscribe::<QueueEvent>();
        for (index, id) in [TrackId(42), TrackId(43)].into_iter().enumerate() {
            assert!(matches!(
                with_outbox(&mut queue, &mut rig, |queue, pass, out| {
                    queue.request_transition(
                        TransitionRequest {
                            id,
                            transition: Transition::None,
                            reason: AdvanceReason::UserSelect,
                            auto: false,
                            playing: true,
                        },
                        Some(pass.output),
                        out,
                    )
                }),
                Err(QueueError::InvalidUrl(_))
            ));
            assert_eq!(
                queue.tracks.records()[index].status,
                TrackStatus::Failed(reason.clone())
            );
            queue.publish();
            assert!(matches!(events.try_recv(), Ok(Envelope {
                event: QueueEvent::TrackStatusChanged { id: changed, status }, ..
            }) if changed == id && status == TrackStatus::Failed(reason.clone())));
        }
        assert!(
            events.try_recv().is_err(),
            "config failure must not emit Loading"
        );
    }
}
