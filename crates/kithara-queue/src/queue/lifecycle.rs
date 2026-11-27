use kithara_bufpool::HasPool;
use kithara_command::{Seq, When};
use kithara_events::TrackId;
use kithara_platform::time::Duration;
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
    F: TrackFactory,
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
        self.read_analysis(id);
    }

    pub(super) fn arm_initial_load(
        &mut self,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_>,
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
        out: &mut Outbox<'_>,
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
        out: &mut Outbox<'_>,
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
        out: &mut Outbox<'_>,
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
        out: &mut Outbox<'_>,
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
        out: &mut Outbox<'_>,
    ) -> Result<(F::Track, Option<Seq>), QueueError> {
        if out.dispatcher_available() == 0 {
            return Err(PlayError::Full("dispatcher").into());
        }
        let (source, initial_position) = self
            .tracks
            .load_spec(id)
            .ok_or(QueueError::UnknownTrackId(id))?;
        let position = held_position.unwrap_or(initial_position);
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
        out: &mut Outbox<'_>,
    ) -> Result<Option<Seq>, QueueError> {
        let staging = (|| {
            if let Some(index) = self.active.replacement_index() {
                self.release_track(index, out)?;
                self.reap_released();
            }
            // A source change continues inside its own slot; another sounding
            // slot cannot receive the continuation.
            let victim = if self.tracks.is_changing_source(id) {
                self.active_current_index()
            } else {
                self.active.quietest(&self.deck.mixer, |_| true)
            };
            let victim = victim.ok_or_else(|| PlayError::InvalidConfiguration {
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

/// The analysis reads of the queue's tracks: started as a track enters the
/// queue and when a committed source changes its request.
impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory,
{
    /// Commit `id`'s staged source. A source with another analysis request
    /// drops the old read and starts its own; the same request keeps the
    /// state the track has.
    pub(super) fn commit_staged_source(&mut self, id: TrackId, duration: Option<Duration>) -> bool {
        let request = self.tracks.analysis_request(id);
        let changed = self.tracks.commit_source(id, duration);
        if changed && self.tracks.analysis_request(id) != request {
            self.read_analysis(id);
        }
        changed
    }

    /// Read `id`'s analysis document for the request its source carries now.
    /// The read runs beside the track: no load or play waits for it, and no
    /// load or replay reads it again.
    pub(super) fn read_analysis(&mut self, id: TrackId) {
        let Some(read) = self.tracks.begin_analysis(id, &self.shutdown) else {
            return;
        };
        let number = read.number;
        let started = self
            .loader
            .as_ref()
            .ok_or(QueueError::NoRuntime)
            .and_then(|loader| loader.read_analysis(read));
        if let Err(error) = started {
            warn!(?id, %error, "the track's analysis document cannot be read");
            self.tracks.place_analysis(id, number, None);
        }
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory,
{
    pub(super) fn release_track(
        &mut self,
        index: usize,
        out: &mut Outbox<'_>,
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

    pub(super) fn release_all(&mut self, out: &mut Outbox<'_>) -> Result<Option<Seq>, PlayError> {
        let detach = self.active.iter().any(|active| active.track.attached());
        let dispatcher = self
            .active
            .tracks()
            .filter(|track| {
                !track.attached() && !track.source_closed() && track.source_request().is_none()
            })
            .count();
        if detach && out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        if out.dispatcher_available() < dispatcher {
            return Err(PlayError::Full("dispatcher"));
        }
        let mut release = |out: &mut Outbox<'_>| {
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

    pub(super) fn close_tracks(&mut self, out: &mut Outbox<'_>) -> Result<Option<Seq>, PlayError> {
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
                    && parked.track.source_closed()
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
            if !active.track.source_closed() {
                self.active.park(Parked {
                    item: active.item,
                    track: active.track,
                    load: active.load,
                });
            }
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
    pub(super) fn pump_loads(&mut self, output: Option<&OutputSnapshot>, out: &mut Outbox<'_>) {
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
        out: &mut Outbox<'_>,
    ) -> Result<(), QueueError> {
        let active = self.active.get(index).ok_or(PlayError::NoActiveSlot)?;
        let id = active.item;
        let position = active.track.snapshot().as_ref().position;
        let (source, _) = self
            .tracks
            .load_spec(id)
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
    use kithara_worker::Worker;

    use super::*;
    use crate::{
        QueueConfig, QueueControl,
        event::QueueEvent,
        queue::{
            QueueCommand,
            state::tests::{
                WorkerOwner, apply, make_queue, wait_for_queue_event, with_outbox, worker_owner,
            },
        },
        test_pools::{TestPools, pools},
    };

    #[cfg(not(target_arch = "wasm32"))]
    type QueueHost = OffThread<Host<TestPools>>;
    #[cfg(target_arch = "wasm32")]
    type QueueHost = Host<TestPools>;

    fn insert_queue(worker: &Worker, host: &mut Host<TestPools>) -> HostOwned<Queue<TestPools>> {
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(pools())
                    .worker(worker.clone())
                    .build(),
            ))
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
    async fn hosted_queue(worker: &Worker) -> (HostOwned<Queue<TestPools>>, QueueHost) {
        hang_tick!();
        let worker = worker.clone();
        let fixture = time::timeout(__hang_detector.remaining(), async move {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let host = OffThread::spawn("queue-lifecycle-host", || {
                    Host::new(HostConfig::offline(pools()).build())
                })
                .await
                .expect("fixture offline Host");
                let queue = host.call(move |host| insert_queue(&worker, host)).await;
                (queue, host)
            }
            #[cfg(target_arch = "wasm32")]
            {
                spawn_blocking(move || {
                    let mut host = Host::new(HostConfig::offline(pools()).build())
                        .expect("fixture offline Host");
                    let queue = insert_queue(&worker, &mut host);
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
    async fn len_is_empty_reflect_append(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
        assert!(queue.is_empty());
        let _ = append(&queue, "https://example.com/a.mp3").await;
        let _ = append(&queue, "https://example.com/b.mp3").await;
        assert_eq!(queue.len(), 2);
        close_host(host).await;
    }

    #[kithara::test(tokio)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn append_returns_monotonic_ids_and_emits_track_added(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn remove_drops_from_queue_and_emits(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn clear_empties_queue(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn clear_discards_old_eof_before_reinsert(#[by_ref] worker_owner: &WorkerOwner) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
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
    async fn set_tracks_replaces_queue(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn insert_after_id_places_next(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn track_source_is_keyed_by_id_across_removal(#[by_ref] worker_owner: &WorkerOwner) {
        let (queue, host) = hosted_queue(&worker_owner.0).await;
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
    async fn begin_dedupes_live_attempt(#[by_ref] worker_owner: &WorkerOwner) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        queue.insert_entry(
            TrackId(1),
            concat!(env!("CARGO_MANIFEST_DIR"), "/missing.wav").into(),
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
    async fn selection_reflects_only_the_requested_live_attempt(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        for id in [TrackId(1), TrackId(2)] {
            queue.insert_entry(
                id,
                concat!(env!("CARGO_MANIFEST_DIR"), "/missing.wav").into(),
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
    async fn promote_waiting_reuses_its_open_and_preserves_its_claim(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        queue.insert_entry(
            TrackId(1),
            concat!(env!("CARGO_MANIFEST_DIR"), "/missing.wav").into(),
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
    async fn promote_keeps_attempt_holding_permit(#[by_ref] worker_owner: &WorkerOwner) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        queue.insert_entry(
            TrackId(1),
            concat!(env!("CARGO_MANIFEST_DIR"), "/missing.wav").into(),
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
    fn promote_vacant_is_noop(#[by_ref] worker_owner: &WorkerOwner) {
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        queue.insert_entry(
            TrackId(1),
            concat!(env!("CARGO_MANIFEST_DIR"), "/missing.wav").into(),
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
    fn config_failure_without_runtime_updates_tracks_synchronously(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        use kithara_events::Envelope;

        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let source: TrackSource<TestPools> = "not a url".into();
        for id in [TrackId(42), TrackId(43)] {
            queue.insert_entry(id, source.clone(), Position::ZERO, Placement::Append);
        }
        queue.publish();
        let output = kithara_play::mock::output(None).get();
        let Err(expected) = queue.loader.as_ref().expect("loader").prepare_source(
            TrackId(42),
            source,
            &output,
            Box::new(kithara_audio::AudioObserverSlot::default().relay()),
        ) else {
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
            assert!(matches!(
                events.try_recv(),
                Ok(Envelope {
                    event: QueueEvent::UpcomingChanged,
                    ..
                })
            ));
        }
        assert!(
            events.try_recv().is_err(),
            "config failure must not emit Loading"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod analysis_tests {
    use std::num::NonZeroU32;

    use axum::{Router, http::StatusCode, routing::get};
    use kithara_analysis::{AnalysisToken, TrackAnalysis};
    use kithara_assets::AssetStore;
    use kithara_audio::{AudioObserver, AudioObserverSlot};
    use kithara_download::{Downloader, DownloaderConfig};
    use kithara_events::EventReceiver;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::{
        CancelToken,
        sync::{Arc, Mutex},
        time::{WallInstant, sleep},
        tokio::{runtime::Handle, sync::watch},
    };
    use kithara_play::{
        ArtifactRequest, ArtifactState, ResourceConfig, ResourceSrc, mock, mock::DeckRig,
    };
    use kithara_test_utils::{TestHttpServer, kithara};

    use super::*;
    use crate::{
        loading::LoadReport,
        queue::{
            QueueCommand,
            state::tests::{WorkerOwner, apply, make_queue, make_store, with_outbox, worker_owner},
        },
        test_pools::{TestPools, pools},
    };

    fn document(revision: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        TrackAnalysis::builder()
            .token(AnalysisToken::from("served"))
            .source_sample_rate(NonZeroU32::new(44_100).expect("BUG: non-zero rate"))
            .media_origin(Duration::ZERO)
            .extent(44_100)
            .settled(true)
            .revision(revision)
            .build()
            .write_to(&mut bytes)
            .expect("BUG: a small snapshot encodes");
        bytes
    }

    /// A one-way signal the tests and the served routes give each other.
    #[derive(Clone)]
    struct Latch(watch::Sender<bool>);

    impl Latch {
        fn new() -> Self {
            Self(watch::channel(false).0)
        }

        fn open(&self) {
            self.0.send_replace(true);
        }

        async fn opened(&self) {
            let mut rx = self.0.subscribe();
            assert!(
                rx.wait_for(|open| *open).await.is_ok(),
                "BUG: the latch outlives its waiters"
            );
        }
    }

    /// Analysis documents served for as long as the server lives: `/a`, `/b`
    /// and `/c` answer at once with revisions 1, 2 and 3, `/held` answers
    /// revision 9 once released, and `/missing` is an error.
    struct Served {
        server: TestHttpServer,
        seen: Arc<Mutex<Vec<&'static str>>>,
        arrived: Latch,
        release: Latch,
    }

    impl Served {
        async fn new() -> Self {
            let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
            let (arrived, release) = (Latch::new(), Latch::new());
            let at_once = |name: &'static str, revision: u64| {
                let seen = Arc::clone(&seen);
                get(move || {
                    seen.lock().push(name);
                    async move { document(revision) }
                })
            };
            let held = {
                let (seen, arrived, release) =
                    (Arc::clone(&seen), arrived.clone(), release.clone());
                get(move || {
                    seen.lock().push("held");
                    arrived.open();
                    let release = release.clone();
                    async move {
                        release.opened().await;
                        document(9)
                    }
                })
            };
            let missing = {
                let seen = Arc::clone(&seen);
                get(move || {
                    seen.lock().push("missing");
                    async { StatusCode::NOT_FOUND }
                })
            };
            let router = Router::new()
                .route("/a", at_once("a", 1))
                .route("/b", at_once("b", 2))
                .route("/c", at_once("c", 3))
                .route("/held", held)
                .route("/missing", missing);
            Self {
                server: TestHttpServer::new(router).await,
                seen,
                arrived,
                release,
            }
        }

        fn url(&self, path: &str) -> ResourceSrc {
            ResourceSrc::Url(self.server.url(path))
        }

        fn hits(&self, name: &str) -> usize {
            self.seen
                .lock()
                .iter()
                .filter(|seen| **seen == name)
                .count()
        }

        async fn arrived(&self) {
            self.arrived.opened().await;
        }
    }

    fn downloader(max_concurrent: usize) -> Downloader {
        let client = HttpClient::new(NetOptions::default(), pools(), CancelToken::never());
        Downloader::new(
            DownloaderConfig::for_client(client)
                .runtime(Handle::current())
                .max_concurrent(max_concurrent)
                .build(),
        )
    }

    /// A track at `audio` whose settings request `analysis`, if any.
    fn source(
        audio: &str,
        analysis: Option<ResourceSrc>,
        store: &AssetStore<TestPools>,
        downloader: &Downloader,
    ) -> TrackSource<TestPools> {
        ResourceConfig::for_src(ResourceSrc::parse(audio).expect("BUG: valid audio URL"))
            .store(store.clone())
            .downloader(downloader.clone())
            .maybe_analysis(analysis.map(|src| ArtifactRequest { src, headers: None }))
            .build()
            .into()
    }

    fn analysis(queue: &Queue<TestPools>, id: TrackId) -> ArtifactState<TrackAnalysis> {
        queue
            .tracks
            .records()
            .iter()
            .find(|record| record.id == id)
            .expect("BUG: the track is queued")
            .entry()
            .analysis()
            .clone()
    }

    fn revision(state: &ArtifactState<TrackAnalysis>) -> Option<u64> {
        match state {
            ArtifactState::Ready(analysis) => Some(analysis.revision()),
            ArtifactState::NotRequested | ArtifactState::Loading | ArtifactState::Failed => None,
        }
    }

    /// Every analysis change the queue announced on `events` since the last
    /// call; the queue announces what it recorded when it publishes.
    fn analysis_changes(
        queue: &mut Queue<TestPools>,
        events: &mut EventReceiver<QueueEvent>,
    ) -> Vec<TrackId> {
        queue.publish();
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|envelope| match envelope.event {
                QueueEvent::TrackAnalysisChanged { id } => Some(id),
                _ => None,
            })
            .collect()
    }

    /// The reports the queue's tasks posted, as they arrive.
    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn next_reports(queue: &mut Queue<TestPools>) -> Vec<QueueCommand<TestPools>> {
        let deadline = WallInstant::now() + Duration::from_secs(5);
        loop {
            hang_tick!();
            let posts: Vec<_> = queue.mailbox.drain().map(|post| post.command).collect();
            if !posts.is_empty() {
                hang_reset!();
                return posts;
            }
            assert!(WallInstant::now() < deadline, "no report arrived");
            sleep(Duration::from_millis(1)).await;
        }
    }

    /// Apply the queue's reports until none of `ids` is loading.
    async fn answered(queue: &mut Queue<TestPools>, rig: &mut DeckRig<TestPools>, ids: &[TrackId]) {
        while ids
            .iter()
            .any(|id| matches!(analysis(queue, *id), ArtifactState::Loading))
        {
            for command in next_reports(queue).await {
                apply(queue, rig, command).expect("BUG: a report applies");
            }
        }
    }

    #[kithara::test(native, tokio)]
    async fn a_track_added_with_a_request_is_read_before_any_play(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let mut events = queue.bus.subscribe::<QueueEvent>();
        let id = TrackId(1);

        queue.insert_entry(
            id,
            source(
                "https://audio.example/a.mp3",
                Some(served.url("/a")),
                &store,
                &downloader,
            ),
            Position::ZERO,
            Placement::Append,
        );
        assert!(
            matches!(analysis(&queue, id), ArtifactState::Loading),
            "the read starts as the track is added, before any load"
        );
        answered(&mut queue, &mut rig, &[id]).await;

        assert_eq!(revision(&analysis(&queue, id)), Some(1));
        assert_eq!(
            analysis_changes(&mut queue, &mut events),
            [id, id],
            "loading, then ready"
        );
        assert!(queue.current.is_none(), "nothing was played");
    }

    /// The document belongs to the track's request, not to a load: loading
    /// the track again and again reads nothing.
    #[kithara::test(native, tokio)]
    async fn loading_a_read_track_again_requests_nothing(#[by_ref] worker_owner: &WorkerOwner) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let id = TrackId(1);
        let track = source(
            "https://audio.example/a.mp3",
            Some(served.url("/a")),
            &store,
            &downloader,
        );
        queue.insert_entry(id, track.clone(), Position::ZERO, Placement::Append);
        answered(&mut queue, &mut rig, &[id]).await;
        let output = mock::output(None).get();
        let observer = || Box::new(AudioObserverSlot::default().relay()) as Box<dyn AudioObserver>;

        for _ in 0..2 {
            let loader = queue
                .loader
                .as_ref()
                .expect("BUG: the queue prepares loads");
            drop(
                loader
                    .start(id, track.clone(), observer(), &output)
                    .expect("BUG: the track loads"),
            );
        }

        assert_eq!(served.hits("a"), 1);
        assert_eq!(revision(&analysis(&queue, id)), Some(1));
    }

    #[kithara::test(native, tokio)]
    async fn a_track_without_a_request_is_not_requested(#[by_ref] worker_owner: &WorkerOwner) {
        let (mut queue, _rig) = make_queue(&worker_owner.0);
        let mut events = queue.bus.subscribe::<QueueEvent>();
        let id = TrackId(1);

        queue.insert_entry(
            id,
            "https://audio.example/a.mp3".into(),
            Position::ZERO,
            Placement::Append,
        );

        assert!(matches!(analysis(&queue, id), ArtifactState::NotRequested));
        assert!(analysis_changes(&mut queue, &mut events).is_empty());
    }

    /// A failed read is final and leaves playback alone: the track is failed
    /// for its analysis and still loads.
    #[kithara::test(native, tokio)]
    async fn a_failed_read_leaves_the_track_playable(#[by_ref] worker_owner: &WorkerOwner) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let id = TrackId(1);
        let track = source(
            "https://audio.example/a.mp3",
            Some(served.url("/missing")),
            &store,
            &downloader,
        );
        queue.insert_entry(id, track.clone(), Position::ZERO, Placement::Append);

        answered(&mut queue, &mut rig, &[id]).await;

        assert!(matches!(analysis(&queue, id), ArtifactState::Failed));
        assert!(
            !matches!(queue.tracks.status(id), Some(TrackStatus::Failed(_))),
            "a failed analysis read failed the track"
        );
        let loader = queue
            .loader
            .as_ref()
            .expect("BUG: the queue prepares loads");
        assert!(
            loader
                .start(
                    id,
                    track,
                    Box::new(AudioObserverSlot::default().relay()),
                    &mock::output(None).get(),
                )
                .is_ok(),
            "a failed analysis read must not stop the track from loading"
        );
        assert_eq!(
            served.hits("missing"),
            1,
            "a failed read is not tried again"
        );
    }

    #[kithara::test(native, tokio)]
    async fn removing_a_track_mid_download_cancels_the_read(#[by_ref] worker_owner: &WorkerOwner) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let mut events = queue.bus.subscribe::<QueueEvent>();
        let id = TrackId(1);
        queue.insert_entry(
            id,
            source(
                "https://audio.example/a.mp3",
                Some(served.url("/held")),
                &store,
                &downloader,
            ),
            Position::ZERO,
            Placement::Append,
        );
        served.arrived().await;
        drop(analysis_changes(&mut queue, &mut events));

        with_outbox(&mut queue, &mut rig, |queue, _, out| {
            queue.remove_entry(id, None, out)
        })
        .expect("BUG: the queued track is removed");
        let reports = next_reports(&mut queue).await;

        assert!(
            matches!(
                reports.as_slice(),
                [QueueCommand::Load(LoadReport::Analysis {
                    analysis: None,
                    ..
                })]
            ),
            "the download was not cancelled: it answered while the server still held it"
        );
        for command in reports {
            apply(&mut queue, &mut rig, command).expect("BUG: a report applies");
        }
        assert!(
            analysis_changes(&mut queue, &mut events).is_empty(),
            "a removed track's read placed its answer"
        );
        served.release.open();
    }

    /// A committed source with another request cancels the old read, whose
    /// answer is then past news, and states the new read.
    #[kithara::test(native, tokio)]
    async fn a_source_with_another_request_drops_the_old_read(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let id = TrackId(1);
        queue.insert_entry(
            id,
            source(
                "https://audio.example/a.mp3",
                Some(served.url("/held")),
                &store,
                &downloader,
            ),
            Position::ZERO,
            Placement::Append,
        );
        served.arrived().await;

        queue
            .tracks
            .stage_source(
                id,
                source(
                    "https://audio.example/b.mp3",
                    Some(served.url("/b")),
                    &store,
                    &downloader,
                ),
                Position::ZERO,
            )
            .expect("BUG: the track takes a replacement");
        assert!(queue.commit_staged_source(id, None));
        answered(&mut queue, &mut rig, &[id]).await;
        served.release.open();
        let late = document(9);
        queue.tracks.apply_report(LoadReport::Analysis {
            id,
            read: 1,
            analysis: Some(Arc::new(
                TrackAnalysis::try_from(late.as_slice()).expect("BUG: the late document reads"),
            )),
        });

        assert_eq!(
            revision(&analysis(&queue, id)),
            Some(2),
            "the old read's late answer replaced the new one"
        );
    }

    #[kithara::test(native, tokio)]
    async fn a_source_with_the_same_request_starts_no_read(#[by_ref] worker_owner: &WorkerOwner) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(5));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let id = TrackId(1);
        queue.insert_entry(
            id,
            source(
                "https://audio.example/a.mp3",
                Some(served.url("/a")),
                &store,
                &downloader,
            ),
            Position::ZERO,
            Placement::Append,
        );
        answered(&mut queue, &mut rig, &[id]).await;
        let mut events = queue.bus.subscribe::<QueueEvent>();

        queue
            .tracks
            .stage_source(
                id,
                source(
                    "https://audio.example/b.mp3",
                    Some(served.url("/a")),
                    &store,
                    &downloader,
                ),
                Position::ZERO,
            )
            .expect("BUG: the track takes a replacement");
        assert!(queue.commit_staged_source(id, None));

        assert_eq!(revision(&analysis(&queue, id)), Some(1));
        assert!(analysis_changes(&mut queue, &mut events).is_empty());
        assert!(queue.mailbox.drain().next().is_none(), "a read started");
        assert_eq!(served.hits("a"), 1);
    }

    /// Reads past the downloader's admission wait for a free slot; every
    /// track ends ready.
    #[kithara::test(native, tokio)]
    async fn more_tracks_than_the_downloader_admits_all_end_ready(
        #[by_ref] worker_owner: &WorkerOwner,
    ) {
        let served = Served::new().await;
        let (store, downloader) = (make_store(), downloader(1));
        let (mut queue, mut rig) = make_queue(&worker_owner.0);
        let tracks = [
            (TrackId(1), "/a", 1),
            (TrackId(2), "/b", 2),
            (TrackId(3), "/c", 3),
        ];
        for (id, path, _) in tracks {
            queue.insert_entry(
                id,
                source(
                    &format!("https://audio.example/{}.mp3", id.as_u64()),
                    Some(served.url(path)),
                    &store,
                    &downloader,
                ),
                Position::ZERO,
                Placement::Append,
            );
        }

        answered(&mut queue, &mut rig, &tracks.map(|(id, ..)| id)).await;

        for (id, _, expected) in tracks {
            assert_eq!(revision(&analysis(&queue, id)), Some(expected));
        }
    }
}
