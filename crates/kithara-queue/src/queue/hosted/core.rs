use std::{num::NonZeroU32, task::Waker};

use kithara_bufpool::HasPool;
use kithara_command::{Batch, Outcome, Receipt, Rejection, Seq};
use kithara_events::{SlotId, TrackId};
use kithara_platform::{maybe_send::MaybeSend, sync::Arc};
use kithara_play::{
    DeckEvent, DeckMixerConfig, DeckPass, DeckProtocol, DeckRefusal, DispatcherProtocol,
    HostedDeck, ItemRole, LoadRefusal, Outbox, OutputSnapshot, PlayError, PlayWorker,
    PlaybackFault, Player, PlayerEvent, ResourceLoad, Settled, TrackCommand, TrackFactory,
    TrackReceipt, TrackRef, TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;
use tracing::warn;

use super::super::{
    Queue, QueueCommand,
    command::play_error,
    slots::{LoadState, Role},
    types::Continuation,
};
use crate::{ActionAtItemEnd, ItemEvent, QueueError, QueueEvent, TrackStatus, loader};

struct DeckSettlement {
    outcomes: Vec<(usize, Settled)>,
    handover: Option<(Option<usize>, Settled)>,
    removal_result: Option<Settled>,
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) fn apply_with_output(
        &mut self,
        command: QueueCommand<S>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let result = self.apply_command(command, output, out).map_err(play_error);
        self.pump_loads(output, out);
        self.publish();
        result
    }

    pub(super) fn settle_with_output(
        &mut self,
        receipt: TrackReceipt<'_, S>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        let mut outcomes: Vec<(usize, Settled)> = Vec::new();
        let mut handover = None;
        let mut removal_result = None;
        match receipt {
            TrackReceipt::Deck {
                seq,
                outcome,
                batch,
            } => {
                let settled = self.settle_deck_receipt(seq, outcome, batch, out);
                outcomes = settled.outcomes;
                handover = settled.handover;
                removal_result = settled.removal_result;
            }
            TrackReceipt::Event(event) => {
                let named: TrackReceipt<'_, S> = TrackReceipt::Event(event);
                for index in self.active.indices(|active| named.names(active.slot)) {
                    if let Some(active) = self.active.get_mut(index) {
                        outcomes
                            .push((index, active.track.settle(TrackReceipt::Event(event), out)));
                    }
                }
                self.item_event(event, output, out);
            }
            TrackReceipt::Loaded(receipt) => {
                let Some(settled) = self.settle_loaded_receipt(receipt, output, out) else {
                    return Settled::Pending;
                };
                outcomes = settled;
            }
        }
        let track_result = self.settle_transitions(outcomes, out);
        let result = if let Some((index, settled)) = handover {
            match self.handover_settled(index, &settled, out) {
                Ok(()) => settled,
                Err(error) => {
                    let seq = match settled {
                        Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => seq,
                        Settled::Pending => return Settled::Pending,
                    };
                    Settled::Rejected {
                        seq,
                        reason: Rejection::Refused(error),
                    }
                }
            }
        } else if let Some(rejected) = removal_result {
            rejected
        } else {
            track_result
        };
        if let Err(error) = self.release_tails(out) {
            warn!(%error, "outgoing queue track could not release");
        }
        if let Err(error) = self.transition_loaded(out) {
            warn!(%error, "loaded queue target could not enter");
        }
        self.reap_released();
        self.publish();
        result
    }

    fn settle_loaded_receipt(
        &mut self,
        receipt: Receipt<DispatcherProtocol<ResourceLoad<S>>>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Option<Vec<(usize, Settled)>> {
        let mut outcomes: Vec<(usize, Settled)> = Vec::new();
        let seq = receipt.seq();
        if let Some(index) = self
            .active
            .position(|active| active.load == Some(LoadState::Opening(seq)))
        {
            let opened = matches!(receipt.outcome(), Outcome::Applied { .. });
            let active = self.active.get(index)?;
            let id = active.item;
            let leaving = active.role == Role::Leaving;
            let wanted = self.target.is_some_and(|target| target.to == id)
                && matches!(active.role, Role::Incoming);
            let host_can_answer_later = self.classify_load(id, leaving, wanted, receipt.outcome());
            if let Some(active) = self.active.get_mut(index) {
                let settled = active.track.settle(TrackReceipt::Loaded(receipt), out);
                if host_can_answer_later {
                    if let Err(error) = self.reopen_requested_load(index, output, out) {
                        let error = play_error(error);
                        outcomes.push((
                            index,
                            Settled::Rejected {
                                seq,
                                reason: Rejection::Refused(error),
                            },
                        ));
                    }
                } else {
                    if opened {
                        self.accept_load(index, seq);
                    }
                    outcomes.push((index, settled));
                }
            }
        } else if let Some(index) = self
            .active
            .parked_position(|parked| parked.load == Some(LoadState::Opening(seq)))
        {
            self.settle_parked(index, receipt, out);
        }
        self.pump_loads(output, out);
        Some(outcomes)
    }

    fn settle_deck_receipt(
        &mut self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &mut Batch<DeckProtocol>,
        out: &mut Outbox<'_, S>,
    ) -> DeckSettlement {
        let mut removal_result = None;
        let removal = self.target.and_then(|target| match target.continued {
            Some(Continuation::Removal {
                seq: removal, slot, ..
            }) if removal == seq && batch.basis.iter().any(|&(named, _)| named == slot) => {
                Some(slot)
            }
            _ => None,
        });
        let returned = removal.is_some_and(|slot| batch.commands.iter().any(|part| matches!(part,
            kithara_play::DeckPart::Returned(kithara_play::Returned::Pcm { slot: named, .. }) if *named == slot)));
        let handover = self.settle_handover(seq, outcome, batch);
        let outcomes = self.settle_deck(seq, outcome, batch, out);
        if returned
            && outcomes.iter().any(|(_, settled)| matches!(settled, Settled::Applied { seq: owned, .. } if *owned == seq))
            && let Some(target) = &mut self.target
            && let Some(Continuation::Removal { seq: removal, slot, applied, .. }) = &mut target.continued
            && *removal == seq
            && batch.basis.iter().any(|&(named, _)| named == *slot)
        {
            *applied = true;
        }
        if removal.is_some()
            && outcomes.iter().any(|(_, settled)| matches!(settled, Settled::Rejected { seq: owned, .. } if *owned == seq))
            && let Outcome::Rejected(reason) = outcome
        {
            removal_result = Some(self.reject_removal(seq, *reason, out));
        }
        DeckSettlement {
            outcomes,
            handover,
            removal_result,
        }
    }

    fn reject_removal(
        &mut self,
        seq: Seq,
        reason: Rejection<DeckRefusal>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        if self
            .target
            .is_some_and(|target| target.reason == crate::AdvanceReason::RemovedCurrent)
        {
            if let Err(error) = self.abandon_removal_entry() {
                warn!(%error, "rejected removal could not retain its neighbor");
            }
        } else if let Err(error) = self.cancel_target(out) {
            warn!(%error, "rejected removal could not cancel its dependent entry");
        }
        Settled::Rejected {
            seq,
            reason: match reason {
                Rejection::Late => Rejection::Late,
                Rejection::Stale => Rejection::Stale,
                Rejection::Unanswered => Rejection::Unanswered,
                Rejection::Refused(refusal) => Rejection::Refused(PlayError::Deck(refusal)),
            },
        }
    }

    fn settle_deck(
        &mut self,
        seq: Seq,
        outcome: &Outcome<DeckProtocol>,
        batch: &mut Batch<DeckProtocol>,
        out: &mut Outbox<'_, S>,
    ) -> Vec<(usize, Settled)> {
        let mut outcomes: Vec<(usize, Settled)> = Vec::new();
        for index in self
            .active
            .indices(|active| batch.basis.iter().any(|&(slot, _)| slot == active.slot))
        {
            if let Some(active) = self.active.get_mut(index) {
                let settled = active.track.settle(
                    TrackReceipt::Deck {
                        seq,
                        outcome,
                        batch: &mut *batch,
                    },
                    out,
                );
                if let Some(super::super::types::SeekPosition::Pending {
                    item,
                    seq: caller,
                    segment,
                    ..
                }) = &mut self.held_position
                    && *item == active.item
                {
                    match settled {
                        Settled::Rejected { seq: answered, .. } if answered == *caller => {
                            self.held_position = None;
                        }
                        Settled::Applied { seq: answered, .. } if answered == *caller => {
                            *segment = batch.commands.iter().find_map(|part| match part {
                                kithara_play::DeckPart::Adopt { slot, segment }
                                    if *slot == active.slot =>
                                {
                                    Some(*segment)
                                }
                                _ => None,
                            });
                        }
                        _ => {}
                    }
                }
                outcomes.push((index, settled));
            }
        }
        outcomes
    }

    fn settle_transitions(
        &mut self,
        outcomes: Vec<(usize, Settled)>,
        out: &mut Outbox<'_, S>,
    ) -> Settled {
        let mut result = Settled::Pending;
        for (index, settled) in outcomes {
            let loading = self
                .active
                .get(index)
                .and_then(|active| active.load)
                .map(LoadState::seq);
            let rescheduling = self.target.and_then(|target| target.stale);
            if let Err(error) = self.transition_settled(index, &settled, out) {
                let seq = match settled {
                    Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => Some(seq),
                    Settled::Pending => None,
                };
                if let Some(seq) = seq {
                    result = Settled::Rejected {
                        seq,
                        reason: Rejection::Refused(error),
                    };
                }
                continue;
            }
            match &settled {
                Settled::Applied { seq, .. } => {
                    if !matches!(result, Settled::Rejected { .. }) && loading != Some(*seq) {
                        result = settled;
                    }
                }
                Settled::Rejected { seq, reason } => {
                    let continued = self.target.is_some_and(|target| {
                        target
                            .continued
                            .is_some_and(|continued| continued.seq() == *seq)
                    });
                    if !continued
                        && (rescheduling != Some(*seq) || !matches!(reason, Rejection::Stale))
                    {
                        result = settled;
                    }
                }
                Settled::Pending => {}
            }
        }
        result
    }

    fn settle_parked(
        &mut self,
        index: usize,
        receipt: Receipt<DispatcherProtocol<ResourceLoad<S>>>,
        out: &mut Outbox<'_, S>,
    ) {
        let Some(parked) = self.active.parked_get_mut(index) else {
            return;
        };
        let id = parked.item;
        let leaving = parked.track.snapshot().as_ref().status == PlayingStatus::Released;
        if matches!(receipt.outcome(), Outcome::Applied { .. }) {
            let seq = receipt.seq();
            parked.track.settle(TrackReceipt::Loaded(receipt), out);
            parked.load = Some(LoadState::Attaching(seq));
            let metadata = parked.track.snapshot().as_ref().metadata.clone();
            if !leaving {
                self.announce_loaded(id, &metadata);
            }
        } else {
            self.classify_load(id, leaving, false, receipt.outcome());
            if let Some(parked) = self.active.parked_get_mut(index) {
                parked.track.settle(TrackReceipt::Loaded(receipt), out);
            }
            self.active.take_parked(index);
        }
    }

    pub(super) fn tick_with_output(
        &mut self,
        now: SessionFrame,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) {
        let parked = self.active.parked_len();
        if let Some((_, delivery)) = self.clock {
            self.clock = Some((now, delivery));
        }
        for active in self.active.iter_mut() {
            active.track.tick(now, out);
        }
        for parked in self.active.parked_iter_mut() {
            parked.track.tick(now, out);
        }
        if let Err(error) = self.tick_deadlines(now, output, out) {
            warn!(%error, "queue deadline could not advance");
        }
        if let Err(error) = self.transition_loaded(out) {
            warn!(%error, "loaded queue target could not enter");
        }
        self.reap_released();
        if self.active.parked_len() < parked {
            self.pump_loads(output, out);
        }
        self.publish();
    }
}

impl<S, F> HostedDeck<S> for Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S> + MaybeSend + 'static,
    F::Track: MaybeSend + 'static,
{
    fn mixer_config(&self) -> DeckMixerConfig {
        self.config.mixer
    }

    delegate::delegate! {
        to self.config.prep {
            #[expr($.map(|prep| &prep.worker))]
            #[call(as_ref)]
            fn worker(&self) -> Option<&PlayWorker<S>>;
            #[call(as_ref)]
            fn resource_prep(&self) -> Option<&kithara_play::ResourcePrep<S>>;
        }
        to self.mailbox {
            fn hold(&mut self, waker: Waker);
            fn release(&mut self);
        }
    }

    #[kithara_test_utils::kithara::hang_watchdog]
    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.accept_host_pass(pass, out);
        for post in self.mailbox.drain() {
            hang_tick!();
            if let Err(error) = self.validate_command(&post.command) {
                self.publish();
                post.answer.answer(Err(error));
                hang_reset!();
                continue;
            }
            match self.apply_with_output(post.command, Some(pass.output), out) {
                Ok(_) => post.answer.answer(Ok(())),
                Err(error) => post.answer.answer(Err(error.into())),
            }
            hang_reset!();
        }
    }

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, S>,
        pass: DeckPass<'_>,
        out: &mut Outbox<'_, S>,
    ) {
        self.accept_host_pass(pass, out);
        self.settle_with_output(receipt, Some(pass.output), out);
    }

    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.accept_host_pass(pass, out);
        self.tick_with_output(pass.now, Some(pass.output), out);
    }

    fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError> {
        self.close_tracks(out)?;
        self.publish();
        Ok(())
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    fn accept_host_pass(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        let had_clock = self.clock.is_some();
        self.clock = out.pass().map(|pass| (pass.now, pass.delivery));
        let rate = pass.output.sample_rate.output();
        if self.deck.mixer.sample_rate != 0
            && self.deck.mixer.sample_rate != rate
            && let Some(rate) = NonZeroU32::new(rate)
            && let Err(error) = self.set_host_rate(rate, out)
        {
            warn!(%error, "queue tracks could not adopt the output rate");
            return;
        }
        self.deck.mix = pass.mix;
        self.deck.suspended = pass.suspended;
        if self.deck.mixer.metrics.event_overflows() != pass.deck.metrics.event_overflows()
            && self.current.is_some()
        {
            self.announce(QueueEvent::CurrentTrackChanged { id: self.current });
        }
        self.deck.mixer.clone_from(pass.deck);
        self.deck.mixer.sample_rate = rate;
        if !had_clock {
            self.arm_initial_load(Some(pass.output), out);
            self.pump_loads(Some(pass.output), out);
        }
    }

    fn set_host_rate(
        &mut self,
        rate: NonZeroU32,
        out: &mut Outbox<'_, S>,
    ) -> Result<(), PlayError> {
        let loaded = self.active.tracks().filter(|track| {
            matches!(
                track.snapshot().as_ref().status,
                PlayingStatus::Loaded
                    | PlayingStatus::Playing { .. }
                    | PlayingStatus::Paused { .. }
                    | PlayingStatus::Faded { .. }
                    | PlayingStatus::Ended { .. }
            )
        });
        let mut attached = 0;
        for track in loaded {
            let snapshot = track.snapshot();
            attached += usize::from(snapshot.as_ref().attached);
            if snapshot.as_ref().lane_room == 0 {
                return Err(PlayError::Full("lane"));
            }
        }
        if out.deck_available() < attached {
            return Err(PlayError::Full("deck"));
        }
        for track in self.active.tracks_mut() {
            if matches!(
                track.snapshot().as_ref().status,
                PlayingStatus::Loaded
                    | PlayingStatus::Playing { .. }
                    | PlayingStatus::Paused { .. }
                    | PlayingStatus::Faded { .. }
                    | PlayingStatus::Ended { .. }
            ) {
                track.apply(TrackCommand::SetHostRate { rate }, out)?;
            }
        }
        Ok(())
    }

    fn classify_load(
        &mut self,
        id: TrackId,
        leaving: bool,
        wanted: bool,
        outcome: &Outcome<DispatcherProtocol<ResourceLoad<S>>>,
    ) -> bool {
        if leaving {
            return false;
        }
        let rejected = match outcome {
            Outcome::Applied { .. } => return false,
            Outcome::Rejected(rejected) => rejected,
        };
        let Rejection::Refused(refusal) = rejected else {
            let error = match rejected {
                Rejection::Late => PlayError::Late,
                Rejection::Stale => PlayError::NotReady,
                Rejection::Unanswered => PlayError::Closed,
                Rejection::Refused(_) => return false,
            };
            self.tracks.fail(id, &error.into());
            return false;
        };
        if matches!(refusal, LoadRefusal::Cancelled) {
            self.tracks.set_status(id, TrackStatus::Cancelled);
            return false;
        }
        let error = QueueError::Resource(refusal.to_string());
        let host_can_answer_later =
            self.tracks
                .refused(id, &error, loader::asks_again(refusal), wanted);
        if !host_can_answer_later {
            self.announce(QueueEvent::TrackLoadFailed {
                id,
                reason: error.to_string(),
                auto_skipped: false,
            });
        }
        host_can_answer_later
    }

    pub(super) fn item_event(
        &mut self,
        event: DeckEvent,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) {
        // The RT reports a gap only after data resumes, so report the stall and
        // its recovery together, in order, only for the current track.
        if let DeckEvent::Underrun { slot, .. } = event {
            if self
                .active
                .iter()
                .any(|active| active.slot == slot && active.role == Role::Current)
            {
                self.bus.publish(ItemEvent::PlaybackStalled);
                self.bus.publish(ItemEvent::PlaybackLikelyToKeepUp);
            }
            return;
        }
        if let DeckEvent::Failed { slot, at, fault } = event {
            let Some(active) = self
                .active
                .iter()
                .find(|active| active.slot == slot && active.role == Role::Current)
            else {
                return;
            };
            let snapshot = active.track.snapshot();
            if !matches!(snapshot.as_ref().status, kithara_play::TrackStatus::Failed { at: failed_at, fault: actual } if failed_at == at && actual == fault)
            {
                return;
            }
            let id = active.item;
            if let Some(item) = self.item_role(slot) {
                self.bus.publish(PlayerEvent::ItemDidFail { item, fault });
            }
            self.item_failed(id, fault, output, out);
            return;
        }
        if let DeckEvent::Ended { slot, at } = event {
            let Some(active) = self.active.iter().find(|active| active.slot == slot) else {
                return;
            };
            if !matches!(active.track.snapshot().as_ref().status, PlayingStatus::Ended { at: ended_at } if ended_at == at)
            {
                return;
            }
            if let Some(item) = self.item_role(slot) {
                self.bus.publish(PlayerEvent::ItemDidPlayToEnd { item });
            }
            let current = self.active.iter().any(|active| {
                active.slot == slot
                    && active.role == Role::Current
                    && self.current == Some(active.item)
            });
            if current
                && self.config.action_at_item_end == ActionAtItemEnd::Pause
                && let Err(error) = self.transport(
                    TrackCommand::Pause {
                        at: kithara_command::When::Next,
                    },
                    out,
                )
            {
                warn!(%error, "queue could not pause its ended track");
            }
            if current && let Err(error) = self.tick_deadlines(at, output, out) {
                warn!(%error, "queue could not settle its ended track");
            }
        }
    }

    fn item_role(&self, slot: kithara_play::Slot) -> Option<ItemRole> {
        let active = self.active.iter().find(|active| active.slot == slot)?;
        let record = self
            .tracks
            .records()
            .iter()
            .find(|record| record.id == active.item)?;
        let track = TrackRef::new(
            active.item,
            SlotId::new(u64::from(slot.get())),
            Arc::from(record.source.uri().unwrap_or_default()),
        );
        Some(match active.role {
            Role::Current => ItemRole::Leading(track),
            Role::Outgoing => ItemRole::Outgoing(track),
            _ => ItemRole::Background(track),
        })
    }

    fn item_failed(
        &mut self,
        id: TrackId,
        fault: PlaybackFault,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) {
        if self.current != Some(id)
            || self
                .track(id)
                .is_none_or(|entry| matches!(entry.status, TrackStatus::Failed(_)))
        {
            return;
        }
        let reason = fault.to_string();
        self.tracks
            .set_status(id, TrackStatus::Failed(reason.clone()));
        self.announce(QueueEvent::TrackLoadFailed {
            id,
            reason,
            auto_skipped: self.config.action_at_item_end == ActionAtItemEnd::Advance,
        });
        if self.target.is_none() && self.config.action_at_item_end == ActionAtItemEnd::Advance {
            match self.next_target(
                super::super::Transition::None,
                crate::AdvanceReason::TrackFailed,
                true,
                true,
                output,
                out,
            ) {
                Ok(Some(_)) => {}
                Ok(None) => self.current = None,
                Err(error) => warn!(%error, "queue could not advance after source failure"),
            }
        }
    }
}

pub(in crate::queue) fn refusal(reason: &Rejection<PlayError>) -> PlayError {
    match reason {
        Rejection::Late => PlayError::Late,
        Rejection::Stale => PlayError::NotReady,
        Rejection::Unanswered => PlayError::Closed,
        Rejection::Refused(error) => error.clone(),
    }
}
