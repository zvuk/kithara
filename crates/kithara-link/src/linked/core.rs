use std::task::Waker;

use kithara_beat::BeatGridModel;
use kithara_command::{Rejection, Seq, When};
use kithara_play::{
    Bound, DeckControl, DeckPass, FadeDir, HostedDeck, Outbox, PlayError, PlayWorker, Player,
    Position, SeekBatch, Settled, Track, TrackCommand, TrackReceipt, TrackSettings,
    TrackSettingsChange, TrackStatus,
};
use kithara_render::{
    bridge::{DeckEvent, DeckPart},
    rt::DeckMixerConfig,
};
use kithara_signal::{FrameCount, SessionFrame};
use tracing::warn;

use super::{
    Linked, LinkedPlayer, LinkedSnapshot, SyncMode, SyncStatus, Waiting,
    geometry::{cue_position, is_final, last_beat, placement},
    state::{Cue, Reissued, Relocation},
};
use crate::{GridAnswer, GridRefusal, TempoTrajectory, covers, entry};

/// Whether `grid` answers a parked command: a Play or Seek is placed by it, and
/// a final grid answers one it cannot place by refusing it.
fn resolves(waiting: Waiting, grid: &BeatGridModel, required: Position) -> bool {
    match waiting {
        Waiting::Play { .. } | Waiting::Seek { .. } => {
            placement(grid, required).is_some() || is_final(grid)
        }
        Waiting::Load { .. } | Waiting::Sync { .. } => covers(grid.geometry(), required),
    }
}

impl<P> Linked<P> {
    fn resume_wait<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(waiting) = self.waiting else {
            return;
        };
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        if matches!(
            track.status,
            TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Released
        ) || self.start.is_some()
        {
            return;
        }
        let Some(pass) = out.pass() else {
            return;
        };
        if self
            .inner
            .entry(Bound::AtOrAfter(pass.earliest()))
            .is_none()
        {
            return;
        }
        let required = match waiting {
            Waiting::Play { .. } | Waiting::Sync { .. } => cue_position(track),
            _ => waiting.required(),
        };
        if self.synced()
            && self
                .grid
                .as_ref()
                .is_none_or(|grid| !resolves(waiting, grid, required))
        {
            if matches!(waiting, Waiting::Sync { .. }) {
                self.waiting = Some(Waiting::Sync { required });
            }
            return;
        }
        self.waiting = None;
        let result = self.apply_waiting(waiting, out);
        match result {
            Ok(_) => {}
            Err(PlayError::Full(_) | PlayError::NotReady | PlayError::Untimed) => {
                self.waiting = Some(waiting);
            }
            Err(error) => {
                warn!(%error, "waiting linked command was refused");
                if matches!(waiting, Waiting::Sync { .. })
                    && matches!(error, PlayError::Internal(_))
                {
                    self.refuse_sync(out);
                }
            }
        }
    }

    fn apply_waiting<S>(
        &mut self,
        waiting: Waiting,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        match waiting {
            Waiting::Play { at, .. } => self.apply(TrackCommand::Play { at }, out),
            Waiting::Seek { required } => self.reissue_seek(required, out),
            Waiting::Sync { .. } if self.synced() => self.enter_sync(out),
            Waiting::Load { .. } | Waiting::Sync { .. } => Ok(None),
        }
    }

    /// Puts the track on the host; a refused alignment restores the mode,
    /// cue and wait the track had.
    fn enter_sync<S>(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let previous = (self.mode, self.cue, self.waiting);
        self.mode = SyncMode::On;
        let result = self.align(out);
        if result.is_err() {
            (self.mode, self.cue, self.waiting) = previous;
        }
        result
    }

    fn reissue_seek<S>(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let sent = self.dispatch_seek(to, out)?;
        if let (Some(relocation), Some(batch)) = (&mut self.relocation, sent) {
            relocation.sent = Some(match batch {
                SeekBatch::Lane(seq) => Reissued::Lane(seq),
                SeekBatch::Deck(seq) => Reissued::Deck { seq, applied: None },
            });
        }
        Ok(sent.map(SeekBatch::seq))
    }

    fn dispatch_seek<S>(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<SeekBatch>, PlayError>
    where
        P: Track<S>,
    {
        if self.synced() {
            self.seek_synced(to, out)
        } else {
            self.inner.seek(to, out)
        }
    }

    pub(super) fn withdraw_start(&mut self, required: Position) {
        if let Some(start) = self.start.take() {
            self.withdrawn.push(start);
            self.waiting = Some(Waiting::Play {
                required,
                at: When::Next,
            });
        }
    }

    pub(super) fn prepare_cue<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        match self.cue_on_beat(out) {
            Ok(_)
            | Err(
                PlayError::Full(_) | PlayError::Late | PlayError::NotReady | PlayError::Untimed,
            ) => {}
            Err(error) => {
                warn!(%error, "synchronized cue refused");
                self.refuse_sync(out);
            }
        }
    }

    pub(super) fn refuse_sync<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        self.mode = SyncMode::Unsyncable;
        self.cue = None;
        self.alignment = None;
        self.resume_wait(out);
    }

    fn retime_silent<S>(&mut self, at: SessionFrame, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(grid) = self.grid.as_ref() else {
            return;
        };
        let snapshot = self.inner.snapshot();
        let required = cue_position(snapshot.as_ref());
        let Ok(source) = self.source(grid) else {
            return;
        };
        let Some(entry_frame) = entry(&self.host, &source, required, Bound::AtOrAfter(at)) else {
            return;
        };
        let curve = match self.curve(&source, required, entry_frame, None) {
            Ok(curve) => curve,
            Err(error) => {
                warn!(?at, %error, "track retime refused");
                self.refuse_sync(out);
                return;
            }
        };
        match self.send_cue(required, curve, out) {
            Ok(Some(seq)) => {
                self.record_retime(at, seq);
                if let Some(start) = self.start {
                    self.start_caller.get_or_insert(start);
                }
                self.withdraw_start(required);
            }
            Ok(None) => {}
            Err(error) => warn!(?at, %error, "track retime refused"),
        }
    }

    pub(super) fn cue_on_beat<S>(
        &mut self,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let Some(cue) = self.cue else {
            return Ok(None);
        };
        let Some(grid) = self.grid.as_ref() else {
            return Ok(None);
        };
        let placed = match cue {
            Cue::Covered(position) => placement(grid, position),
            Cue::Beat(position) => kithara_sync::boundary(grid.geometry(), position),
        };
        let Some(position) = placed.or_else(|| last_beat(grid)) else {
            if is_final(grid) {
                return Err(PlayError::Internal(
                    "the final grid holds no beat for this cue".into(),
                ));
            }
            return Ok(None);
        };
        let Some(pass) = out.pass() else {
            return Ok(None);
        };
        let source = self.source(grid)?;
        let Some(at) = entry(
            &self.host,
            &source,
            position,
            Bound::AtOrAfter(pass.deck_earliest()),
        ) else {
            return Ok(None);
        };
        let value = self.curve(&source, position, at, None)?;
        tracing::debug!(
            item = u64::from(self.inner.snapshot().as_ref().item),
            ?position,
            entry = i64::from(at),
            speed = ?value,
            "preparing synchronized load cue"
        );
        let sent = self.send_cue(position, value, out)?;
        self.cue = None;
        self.withdraw_start(position);
        self.waiting = self.waiting.and_then(|waiting| match waiting {
            Waiting::Load { .. } => Some(Waiting::Load { required: position }),
            Waiting::Play { at, .. } => Some(Waiting::Play {
                required: position,
                at,
            }),
            Waiting::Sync { .. } => None,
            Waiting::Seek { .. } => Some(waiting),
        });
        Ok(sent)
    }
}

impl<S, P: Track<S>> Player<S> for Linked<P> {
    type Command = TrackCommand<S>;
    type Snapshot = LinkedSnapshot<P::Snapshot>;

    fn entry(&self, bound: Bound) -> Option<SessionFrame> {
        if !self.synced() {
            return self.inner.entry(bound);
        }
        self.inner.entry(bound)?;
        let track = self.inner.snapshot();
        if matches!(
            track.as_ref().status,
            TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Released
        ) {
            return None;
        }
        self.grid.as_ref().and_then(|grid| {
            let position = cue_position(track.as_ref());
            entry(&self.host, &self.source(grid).ok()?, position, bound)
        })
    }

    fn apply(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_synced_command(&command, out)?;
        match command {
            TrackCommand::Pause { .. } => {
                self.paused_in_place |= matches!(
                    self.inner.snapshot().as_ref().status,
                    TrackStatus::Playing { .. } | TrackStatus::Faded { .. }
                );
            }
            TrackCommand::Seek { .. } => self.paused_in_place = false,
            _ => {}
        }
        if let TrackCommand::Load { item, position } = command {
            return self.load_item(item, position, out);
        }
        if !self.synced() {
            return self.inner.apply(command, out);
        }
        match command {
            TrackCommand::Load { .. } => unreachable!("load was handled before the SYNC dispatch"),
            TrackCommand::Configure(change, at) => {
                self.synced_speed(change)?;
                self.inner.apply(TrackCommand::Configure(change, at), out)
            }
            TrackCommand::Play { at } => self.play_synced(at, out),
            TrackCommand::Seek { to } => Ok(Track::seek(self, to, out)?.map(SeekBatch::seq)),
            TrackCommand::Pause { at } => {
                self.waiting = None;
                self.inner.apply(TrackCommand::Pause { at }, out)
            }
            TrackCommand::Fade { at, settings, dir } => {
                self.check_synced_fade::<S>(at, dir)?;
                let sent = self
                    .inner
                    .apply(TrackCommand::Fade { at, settings, dir }, out)?;
                if dir == FadeDir::Out && matches!(self.waiting, Some(Waiting::Play { .. })) {
                    self.waiting = None;
                }
                if dir == FadeDir::In {
                    self.entry_planned::<S>();
                }
                Ok(sent)
            }
            other => self.inner.apply(other, out),
        }
    }

    fn settle(&mut self, receipt: TrackReceipt<'_, S>, out: &mut Outbox<'_, S>) -> Settled {
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        let slot = track.slot;
        let underrun = track.attached
            && matches!(track.status, TrackStatus::Playing { .. })
            && matches!(&receipt,
                TrackReceipt::Event(DeckEvent::UnderrunEnded { slot: named, item, recovered: true, .. })
                    if Some(*named) == slot && *item == track.item
            );
        let mut settled = self.inner.settle(receipt, out);
        self.answer_reissued(&settled);
        if let Settled::Rejected {
            seq,
            reason: Rejection::Stale,
        } = &settled
            && let Some(index) = self.withdrawn.iter().position(|withdrawn| withdrawn == seq)
        {
            self.withdrawn.remove(index);
            return Settled::Pending;
        }
        if matches!(&settled,
            Settled::Applied { seq, .. } | Settled::Rejected { seq, .. }
                if self.start == Some(*seq)
        ) {
            self.start = None;
            if let Some(caller) = self.start_caller.take() {
                match &mut settled {
                    Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => {
                        *seq = caller;
                    }
                    Settled::Pending => {}
                }
            }
        }
        if self.synced() && underrun {
            self.alignment = Some(false);
            self.realign(out);
        }
        if self.cue.is_none() && matches!(self.waiting, Some(Waiting::Load { .. })) {
            self.resume_wait(out);
        }
        settled
    }

    fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, S>) {
        self.inner.tick(now, out);
        self.drain_speed_receipts::<S>();
        self.update_correction::<S>();
        if self.synced() {
            self.realign(out);
        }
        if self.synced() {
            self.prepare_cue(out);
        }
        if self.cue.is_none() {
            self.resume_wait(out);
        }
    }

    fn snapshot(&self) -> Self::Snapshot {
        let track = self.inner.snapshot();
        let remaining = self.correction.as_ref().and_then(|(_, correction)| {
            let mark = track
                .as_ref()
                .mark
                .filter(|mark| mark.lane.segment == correction.segment)?;
            let elapsed = usize::try_from(mark.session.frames_since(correction.at)?).ok()?;
            let frames = correction.frames.get().checked_sub(elapsed)?;
            (frames > 0).then_some(FrameCount::new(frames))
        });
        let sync = match self.mode {
            SyncMode::Off => SyncStatus::Off,
            SyncMode::Unsyncable => SyncStatus::Unsyncable,
            SyncMode::On => self.waiting.map_or_else(
                || {
                    remaining.map_or(SyncStatus::On, |remaining| SyncStatus::Correcting {
                        remaining,
                    })
                },
                |waiting| SyncStatus::WaitingForGrid {
                    required: waiting.required(),
                },
            ),
        };
        LinkedSnapshot { track, sync }
    }
}

impl<S, P: Track<S>> Track<S> for Linked<P> {
    fn admit(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<Option<usize>, PlayError> {
        self.synced_speed(change)?;
        self.inner.admit(change, at, out)
    }

    fn prepare_entry(
        &mut self,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<bool, PlayError> {
        if !self.inner.prepare_entry(at, out)? {
            return Ok(false);
        }
        if !self.synced() {
            return Ok(true);
        }
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        if matches!(track.status, TrackStatus::Playing { .. }) {
            return Ok(true);
        }
        if matches!(
            track.status,
            TrackStatus::Idle | TrackStatus::Loading | TrackStatus::Released
        ) || self.cue.is_some()
        {
            return Ok(false);
        }
        let ended = matches!(track.status, TrackStatus::Ended { .. });
        let position = cue_position(track);
        let Some(grid) = self
            .grid
            .as_ref()
            .filter(|grid| covers(grid.geometry(), position))
        else {
            return Ok(false);
        };
        let source = self.source(grid)?;
        let curve = self.curve(&source, position, at, None)?;
        if !ended && self.inner.prepared_speed() == Some(&curve) {
            return Ok(true);
        }
        tracing::debug!(
            item = u64::from(track.item),
            ?position,
            entry = i64::from(at),
            prepared = ?self.inner.prepared_speed(),
            speed = ?curve,
            "repreparing silent linked entry"
        );
        self.send_cue(position, curve, out)?;
        Ok(false)
    }

    fn cue(
        &mut self,
        position: Position,
        speed: kithara_sync::SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.paused_in_place = false;
        self.send_cue(position, speed, out)
    }

    fn seek(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<SeekBatch>, PlayError> {
        self.relocation = None;
        self.paused_in_place = false;
        self.dispatch_seek(to, out)
    }

    fn seek_applied(&mut self, seq: Seq) -> Option<bool> {
        self.drain_speed_receipts::<S>();
        match self.relocation {
            Some(Relocation {
                caller,
                sent: Some(Reissued::Lane(sent)),
            }) if caller == seq => self.inner.seek_applied(sent),
            Some(Relocation {
                caller,
                sent: Some(Reissued::Deck { applied, .. }),
            }) if caller == seq => applied,
            Some(Relocation { caller, sent: None }) if caller == seq => {
                if matches!(self.waiting, Some(Waiting::Seek { .. })) {
                    None
                } else {
                    Some(false)
                }
            }
            _ => self.inner.seek_applied(seq),
        }
    }

    fn follows_host(&self) -> bool {
        self.mode == SyncMode::On
    }

    delegate::delegate! {
        to self.inner {
            fn projected(&self) -> TrackSettings;
            fn prepared_speed(&self) -> Option<&kithara_sync::SpeedCurve>;
            fn planned(
                &self,
                at: SessionFrame,
                sample_rate: std::num::NonZeroU32,
            ) -> Result<(Position, f64), PlayError>;
            fn planned_end(
                &self,
                sample_rate: std::num::NonZeroU32,
            ) -> Result<Option<SessionFrame>, PlayError>;
            fn speed_receipt(&mut self) -> Option<Settled>;
            fn speed_applied(&mut self, seq: Seq) -> Option<bool>;
            fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>);
        }
    }
}

impl<P: DeckControl> DeckControl for Linked<P> {
    type Control = P::Control;

    fn control(&self) -> Self::Control {
        self.inner.control()
    }
}

impl<S, P> HostedDeck<S> for Linked<P>
where
    P: Track<S> + HostedDeck<S>,
{
    delegate::delegate! {
        to self.inner {
            fn worker(&self) -> Option<&PlayWorker<S>>;
            fn resource_prep(&self) -> Option<&kithara_play::ResourcePrep<S>>;
            fn mixer_config(&self) -> DeckMixerConfig;
            fn drain(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>);
            fn close(&mut self, out: &mut Outbox<'_, S>) -> Result<(), PlayError>;
            fn hold(&mut self, waker: Waker);
            fn release(&mut self);
        }
    }

    fn settle(
        &mut self,
        receipt: TrackReceipt<'_, S>,
        _pass: DeckPass<'_>,
        out: &mut Outbox<'_, S>,
    ) {
        Player::settle(self, receipt, out);
    }

    fn tick(&mut self, pass: DeckPass<'_>, out: &mut Outbox<'_, S>) {
        Player::tick(self, pass.now, out);
    }
}

impl<P> Linked<P> {
    fn send_cue<S>(
        &mut self,
        position: Position,
        speed: kithara_sync::SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let sent = self.inner.cue(position, speed, out)?;
        if let Some(seq) = sent {
            for (_, current) in &mut self.retimes {
                *current = seq;
            }
        }
        Ok(sent)
    }
}

impl<S, P: Track<S>> LinkedPlayer<S> for Linked<P> {
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        if !on {
            self.mode = SyncMode::Off;
            self.waiting = None;
            self.cue = None;
            self.alignment = None;
            return Ok(None);
        }
        if self.mode == SyncMode::On {
            return Ok(None);
        }
        self.enter_sync(out)
    }

    fn retime(
        &mut self,
        trajectory: &TempoTrajectory,
        frames: &[SessionFrame],
        out: &mut Outbox<'_, S>,
    ) {
        self.host = trajectory.clone();
        if !self.synced() || self.grid.is_none() {
            return;
        }
        if matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            self.retime_sounding(frames, out);
            return;
        }
        for &at in frames {
            if self.synced() {
                self.retime_silent(at, out);
            }
        }
    }

    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>) {
        if self.inner.snapshot().as_ref().item != answer.item {
            return;
        }
        let validated = answer.model.and_then(|model| {
            self.source(&model).map_err(|error| GridRefusal {
                reason: error.to_string(),
            })?;
            Ok(model)
        });
        match validated {
            Ok(model) => {
                if self.grid.as_ref() == Some(&model) {
                    return;
                }
                let refused_for_lack_of_a_grid =
                    self.mode == SyncMode::Unsyncable && self.grid.is_none();
                self.grid = Some(model);
                if refused_for_lack_of_a_grid {
                    self.mode = SyncMode::On;
                    let required = cue_position(self.inner.snapshot().as_ref());
                    self.waiting.get_or_insert(Waiting::Sync { required });
                    return;
                }
                if self.synced() {
                    if matches!(
                        self.inner.snapshot().as_ref().status,
                        TrackStatus::Playing { .. }
                    ) && self.waiting.is_none()
                    {
                        self.alignment = Some(false);
                        self.realign(out);
                        return;
                    }
                    if self.start.is_some() {
                        self.cue = Some(Cue::Beat(self.inner.snapshot().as_ref().position));
                    } else if !matches!(
                        self.inner.snapshot().as_ref().status,
                        TrackStatus::Playing { .. }
                    ) {
                        self.alignment = Some(false);
                    }
                    self.prepare_cue(out);
                    if self.cue.is_none() {
                        self.resume_wait(out);
                    }
                }
            }
            Err(refusal) => {
                warn!(item = ?answer.item, %refusal, "track grid unavailable");
                self.grid = None;
                if self.synced() {
                    self.refuse_sync(out);
                }
            }
        }
    }

    fn synced(&self) -> bool {
        self.mode == SyncMode::On
    }

    fn lead(&self, delivery: FrameCount) -> Option<FrameCount> {
        if !self.synced()
            || !matches!(
                self.inner.snapshot().as_ref().status,
                TrackStatus::Playing { .. }
            )
        {
            return None;
        }
        let snapshot = self.inner.snapshot();
        let track = snapshot.as_ref();
        Some(FrameCount::new(
            delivery
                .get()
                .saturating_add(track.ring_depth.get())
                .saturating_add(track.engine_latency.get()),
        ))
    }

    fn lane_room(&self) -> usize {
        if !self.synced() || self.grid.is_none() {
            usize::MAX
        } else {
            self.inner.snapshot().as_ref().lane_room
        }
    }

    fn scope_parts(&self) -> usize {
        let snapshot = self.inner.snapshot();
        usize::from(
            self.synced()
                && self.grid.is_some()
                && snapshot.as_ref().attached
                && !matches!(snapshot.as_ref().status, TrackStatus::Playing { .. }),
        )
    }

    fn retime_applied(&mut self, at: SessionFrame) -> Option<bool> {
        let mut awaiting = false;
        for &(frame, seq) in &self.retimes {
            if frame != at {
                continue;
            }
            match self.inner.speed_applied(seq) {
                Some(true) => return Some(true),
                None => awaiting = true,
                Some(false) => {}
            }
        }
        (!awaiting).then_some(false)
    }

    fn realign(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        if !matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            self.retime(trajectory, &[at], out);
            return;
        }
        self.host = trajectory.clone();
        if self.synced()
            && let Err(error) = self.correct(at, false, out)
        {
            self.alignment = Some(false);
            warn!(?at, %error, "tempo phase correction awaits a lane");
        }
    }
}
