use std::{num::NonZeroU32, task::Waker};

use kithara_bufpool::HasPool;
use kithara_command::{Outcome, Receipt, Rejection, Seq};
use kithara_events::TrackId;
use kithara_platform::maybe_send::MaybeSend;
use kithara_play::{
    DeckEvent, DeckMixerConfig, DeckPass, HostedDeck, LoadRefusal, Outbox, OutputSnapshot,
    PlayError, PlayWorker, Player, Settled, TrackCommand, TrackFactory, TrackReceipt,
    TrackStatus as PlayingStatus,
};
use kithara_signal::SessionFrame;
use tracing::warn;

use super::super::{
    Queue, QueueCommand,
    command::play_error,
    slots::{LoadState, Role},
};
use crate::{ActionAtItemEnd, ItemEvent, QueueError, QueueEvent, TrackStatus, loader};

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
        match receipt {
            TrackReceipt::Deck {
                seq,
                outcome,
                batch,
            } => {
                for index in self
                    .active
                    .indices(|active| batch.basis.iter().any(|&(slot, _)| slot == active.slot))
                {
                    if let Some(active) = self.active.get_mut(index) {
                        outcomes.push((
                            index,
                            active.track.settle(
                                TrackReceipt::Deck {
                                    seq,
                                    outcome,
                                    batch: &mut *batch,
                                },
                                out,
                            ),
                        ));
                    }
                }
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
                let seq = receipt.seq();
                if let Some(index) = self
                    .active
                    .position(|active| active.load == Some(LoadState::Opening(seq)))
                {
                    let opened = matches!(receipt.outcome(), Outcome::Applied { .. });
                    let Some(active) = self.active.get(index) else {
                        return Settled::Pending;
                    };
                    let id = active.item;
                    let leaving = active.role == Role::Leaving;
                    let wanted = self.target.is_some_and(|target| target.to == id)
                        && matches!(active.role, Role::Incoming { .. });
                    let retry = self.classify_load(id, leaving, wanted, receipt.outcome());
                    if let Some(active) = self.active.get_mut(index) {
                        let settled = active.track.settle(TrackReceipt::Loaded(receipt), out);
                        if retry {
                            if let Err(error) = self.retry_load(index, output, out) {
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
            }
        }
        let result = self.settle_transitions(outcomes, out);
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
                    let retrying = self.target.is_some_and(|target| target.retry == Some(*seq));
                    if !retrying
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
        receipt: Receipt<kithara_play::DispatcherProtocol<kithara_play::ResourceLoad<S>>>,
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

    fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        self.accept_host_pass(pass, out);
        for post in self.mailbox.drain() {
            if let Err(error) = self.validate_command(&post.command) {
                self.publish();
                post.answer.answer(Err(error));
                continue;
            }
            match self.apply_with_output(post.command, Some(pass.output), out) {
                Ok(_) => post.answer.answer(Ok(())),
                Err(error) => post.answer.answer(Err(error.into())),
            }
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
        outcome: &Outcome<kithara_play::DispatcherProtocol<kithara_play::ResourceLoad<S>>>,
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
        let retry = self
            .tracks
            .refused(id, &error, loader::asks_again(refusal), wanted);
        if !retry {
            self.announce(QueueEvent::TrackLoadFailed {
                id,
                reason: error.to_string(),
                auto_skipped: false,
            });
        }
        retry
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
            if self
                .track(id)
                .is_some_and(|entry| matches!(entry.status, TrackStatus::Failed(_)))
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
                    Ok(None) => self.announce(QueueEvent::QueueEnded),
                    Err(error) => warn!(%error, "queue could not advance after source failure"),
                }
            }
            return;
        }
        if let DeckEvent::Ended { slot, .. } = event {
            let current = self
                .active
                .iter()
                .any(|active| active.slot == slot && active.role == Role::Current);
            if current
                && self.target.is_none()
                && self.config.action_at_item_end == ActionAtItemEnd::Advance
            {
                let ids = self.track_ids();
                let wrap = self.navigation.repeat_mode() == crate::RepeatMode::All;
                if self.navigation.next(&ids, true, wrap).is_none() {
                    self.announce(QueueEvent::QueueEnded);
                }
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
