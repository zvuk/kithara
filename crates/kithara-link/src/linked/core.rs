use std::task::Waker;

use kithara_command::{Rejection, Seq, When};
use kithara_play::{
    Bound, DeckControl, DeckPass, HostedDeck, Outbox, PlayError, PlayWorker, Player, Position,
    Settled, Track, TrackCommand, TrackReceipt, TrackSettings, TrackSettingsChange, TrackStatus,
};
use kithara_render::{
    bridge::{DeckEvent, DeckPart},
    rt::DeckMixerConfig,
};
use kithara_signal::{FrameCount, SessionFrame};
use kithara_sync::corrected_speed_curve;
use tracing::warn;

use super::{
    Linked, LinkedPlayer, LinkedSnapshot, SyncMode, SyncStatus, Waiting,
    geometry::cue_position,
    state::{CorrectionState, RunningCorrection, SpeedOperation},
};
use crate::{GridAnswer, TempoTrajectory, covers, entry, jump_target};

impl<P> Linked<P> {
    fn drain_speed_receipts<S>(&mut self)
    where
        P: Track<S>,
    {
        if !self.speeds.is_empty() {
            while let Some(receipt) = self.inner.speed_receipt() {
                let seq = match &receipt {
                    Settled::Pending => continue,
                    Settled::Applied { seq, .. } | Settled::Rejected { seq, .. } => *seq,
                };
                let Some(index) = self.speeds.iter().position(|(pending, _)| *pending == seq)
                else {
                    continue;
                };
                if let Settled::Applied { at, .. } = receipt {
                    if let SpeedOperation::Correction { state, .. } = &mut self.speeds[index].1 {
                        if let CorrectionState::Pending(correction) = state {
                            *state =
                                CorrectionState::Applied(RunningCorrection { at, ..*correction });
                        }
                    } else {
                        self.speeds.remove(index);
                    }
                    continue;
                }
                let (_, operation) = self.speeds.remove(index);
                if self.synced()
                    && matches!(
                        receipt,
                        Settled::Rejected {
                            reason: Rejection::Late | Rejection::Refused(_),
                            ..
                        }
                    )
                {
                    match operation {
                        SpeedOperation::Correction { commanded, .. } => {
                            self.alignment = Some(commanded);
                        }
                        SpeedOperation::Alignment => {
                            self.waiting = Some(Waiting::Sync {
                                required: self.inner.snapshot().as_ref().position,
                            });
                        }
                        SpeedOperation::Relocation { to } => {
                            self.waiting = Some(Waiting::Seek { required: to });
                        }
                    }
                }
            }
        }
    }

    fn record_retime(&mut self, at: SessionFrame, seq: Seq) {
        self.retimes.retain(|(frame, _)| *frame != at);
        self.retimes.push((at, seq));
    }

    fn correct<S>(
        &mut self,
        at: SessionFrame,
        commanded: bool,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let Some(grid) = self.grid.as_ref() else {
            return Err(PlayError::NotReady);
        };
        let segment = self
            .inner
            .snapshot()
            .as_ref()
            .mark
            .ok_or(PlayError::NotReady)?
            .lane
            .segment;
        let (position, from) = self.planned::<S>(at)?;
        if !covers(grid.geometry(), position) {
            return Err(PlayError::NotReady);
        }
        if self.lane_room() == 0 {
            return Err(PlayError::Full("lane"));
        }
        let source = self.source::<S>(grid)?;
        let (curve, frame_count) = corrected_speed_curve(
            &self.host,
            &source,
            position,
            at,
            from,
            self.config.epsilon(),
            commanded,
        )
        .ok_or_else(|| {
            PlayError::Internal("source phase correction has no playable grid law".into())
        })?;
        let frames = usize::try_from(frame_count)
            .map(FrameCount::new)
            .map_err(|error| PlayError::Internal(error.to_string()))?;
        let sent = self.inner.apply(
            TrackCommand::SetSpeed {
                speed: curve,
                at: When::At(at),
            },
            out,
        )?;
        if let Some(seq) = sent {
            self.speeds.push((
                seq,
                SpeedOperation::Correction {
                    commanded,
                    state: CorrectionState::Pending(RunningCorrection {
                        at,
                        frames,
                        segment,
                    }),
                },
            ));
        }
        Ok(sent)
    }

    fn realign<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(commanded) = self.alignment else {
            return;
        };
        let result = self
            .earliest(out)
            .and_then(|at| self.correct(at, commanded, out));
        match result {
            Ok(_) => self.alignment = None,
            Err(PlayError::Full(_) | PlayError::NotReady | PlayError::Untimed) => {}
            Err(error) => warn!(%error, "track realignment refused"),
        }
    }

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
                .is_none_or(|grid| !covers(grid.geometry(), required))
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
            Err(error) => warn!(%error, "waiting linked command was refused"),
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
            Waiting::Seek { required } => self.apply(TrackCommand::Seek { to: required }, out),
            Waiting::Sync { .. } if self.synced() => self.sync(true, out),
            Waiting::Load { .. } | Waiting::Sync { .. } => Ok(None),
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

    fn prepare_cue<S>(&mut self, out: &mut Outbox<'_, S>)
    where
        P: Track<S>,
    {
        let Some(required) = self.cue else {
            return;
        };
        let Some(grid) = self.grid.as_ref() else {
            return;
        };
        let Some(position) = kithara_sync::boundary(grid.geometry(), required) else {
            return;
        };
        let Some(pass) = out.pass() else {
            return;
        };
        let Ok(source) = self.source::<S>(grid) else {
            return;
        };
        let Some(at) = entry(
            &self.host,
            &source,
            position,
            Bound::AtOrAfter(pass.earliest()),
        ) else {
            return;
        };
        let Ok(value) = self.curve(&source, position, at, None) else {
            return;
        };
        tracing::debug!(
            item = u64::from(self.inner.snapshot().as_ref().item),
            load = ?self.load,
            ?required,
            ?position,
            entry = i64::from(at),
            speed = ?value,
            "preparing synchronized load cue"
        );
        match self.cue(position, value, out) {
            Ok(_) => {
                self.cue = None;
                self.withdraw_start(position);
                self.waiting = self.waiting.map(|waiting| match waiting {
                    Waiting::Load { .. } => Waiting::Load { required: position },
                    Waiting::Play { at, .. } => Waiting::Play {
                        required: position,
                        at,
                    },
                    other => other,
                });
            }
            Err(error) => warn!(%error, "synchronized cue waits for its segment"),
        }
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
            entry(&self.host, &self.source::<S>(grid).ok()?, position, bound)
        })
    }

    fn apply(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.check_synced_command(&command, out)?;
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
            TrackCommand::Seek { to } => self.seek_synced(to, out),
            TrackCommand::Pause { at } => {
                self.waiting = None;
                self.inner.apply(TrackCommand::Pause { at }, out)
            }
            TrackCommand::Fade { at, settings, dir } => {
                self.check_synced_fade::<S>(at, dir)?;
                self.inner
                    .apply(TrackCommand::Fade { at, settings, dir }, out)
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
                TrackReceipt::Event(DeckEvent::Underrun { slot: named, .. }) if Some(*named) == slot
            );
        let mut settled = self.inner.settle(receipt, out);
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
        let source = self.source::<S>(grid)?;
        let curve = self.curve(&source, position, at, None)?;
        if !ended && self.inner.prepared_speed() == Some(&curve) {
            return Ok(true);
        }
        tracing::debug!(
            item = u64::from(track.item),
            load = ?self.load,
            ?position,
            entry = i64::from(at),
            prepared = ?self.inner.prepared_speed(),
            speed = ?curve,
            "repreparing silent linked entry"
        );
        self.cue(position, curve, out)?;
        Ok(false)
    }

    fn cue(
        &mut self,
        position: Position,
        speed: kithara_sync::SpeedCurve,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let sent = self.inner.cue(position, speed, out)?;
        if let Some(seq) = sent {
            for (_, current) in &mut self.retimes {
                *current = seq;
            }
        }
        Ok(sent)
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
    fn align_playing<S>(
        &mut self,
        source: &kithara_sync::SourceGrid,
        position: Position,
        at: SessionFrame,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        if self.lane_room() == 0 {
            return Err(PlayError::Full("lane"));
        }
        if out.deck_available() == 0 {
            return Err(PlayError::Full("deck"));
        }
        let to = jump_target(&self.host, source, position, at).ok_or(PlayError::NotReady)?;
        let start = at - self.inner.snapshot().as_ref().declick;
        let (before, _) = self.planned::<S>(start)?;
        let sent = self.inner.apply(
            TrackCommand::Align {
                to,
                speed: self.curve(source, before, start, Some((to, at)))?,
                at,
            },
            out,
        )?;
        if let Some(seq) = sent {
            self.speeds.push((seq, SpeedOperation::Alignment));
        }
        self.correction = None;
        self.waiting = None;
        Ok(sent)
    }

    fn align<S>(&mut self, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError>
    where
        P: Track<S>,
    {
        let track = self.inner.snapshot();
        let required = cue_position(track.as_ref());
        let playing = matches!(track.as_ref().status, TrackStatus::Playing { .. });
        let Some(grid) = self
            .grid
            .as_ref()
            .filter(|grid| playing || covers(grid.geometry(), required))
        else {
            self.waiting = Some(Waiting::Sync { required });
            return Ok(None);
        };
        let source = self.source::<S>(grid)?;
        if playing {
            let at = self.jump_frame(out)?;
            let (position, _speed) = self.planned::<S>(at)?;
            if !covers(grid.geometry(), position) {
                self.waiting = Some(Waiting::Sync { required: position });
                return Ok(None);
            }
            return self.align_playing(&source, position, at, out);
        }
        let at = self.earliest(out)?;
        let at = entry(&self.host, &source, required, Bound::AtOrAfter(at))
            .ok_or(PlayError::NotReady)?;
        self.cue(required, self.curve(&source, required, at, None)?, out)
    }
}

impl<S, P: Track<S>> LinkedPlayer<S> for Linked<P> {
    fn sync(&mut self, on: bool, out: &mut Outbox<'_, S>) -> Result<Option<Seq>, PlayError> {
        let previous = self.mode;
        self.mode = if on { SyncMode::On } else { SyncMode::Off };
        if !on {
            self.waiting = None;
            self.cue = None;
            self.alignment = None;
            return Ok(None);
        }
        let result = self.align(out);
        if result.is_err() {
            self.mode = previous;
        }
        result
    }

    fn retime(&mut self, trajectory: &TempoTrajectory, at: SessionFrame, out: &mut Outbox<'_, S>) {
        self.host = trajectory.clone();
        if !self.synced() {
            return;
        }
        let Some(grid) = self.grid.as_ref() else {
            return;
        };
        if matches!(
            self.inner.snapshot().as_ref().status,
            TrackStatus::Playing { .. }
        ) {
            match self.correct(at, true, out) {
                Ok(Some(seq)) => self.record_retime(at, seq),
                Ok(None) => {}
                Err(PlayError::Late) => {
                    self.alignment = Some(true);
                    self.realign(out);
                }
                Err(PlayError::Full(_) | PlayError::NotReady | PlayError::Untimed) => {
                    self.alignment = Some(true);
                }
                Err(error) => warn!(?at, %error, "track retime refused"),
            }
            return;
        }
        let snapshot = self.inner.snapshot();
        let required = cue_position(snapshot.as_ref());
        let Ok(source) = self.source::<S>(grid) else {
            return;
        };
        let Some(entry_frame) = entry(&self.host, &source, required, Bound::AtOrAfter(at)) else {
            return;
        };
        let Ok(curve) = self.curve(&source, required, entry_frame, None) else {
            return;
        };
        match self.cue(required, curve, out) {
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

    fn grid(&mut self, answer: GridAnswer, out: &mut Outbox<'_, S>) {
        if self.load != Some(answer.load) || self.inner.snapshot().as_ref().item != answer.item {
            return;
        }
        match answer.model {
            Ok(model) => {
                if self.grid.as_ref() == Some(&model) {
                    return;
                }
                self.grid = Some(model);
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
                        self.cue = Some(self.inner.snapshot().as_ref().position);
                    }
                    self.prepare_cue(out);
                    if self.cue.is_none() {
                        self.resume_wait(out);
                    }
                }
            }
            Err(refusal) => {
                warn!(item = ?answer.item, load = ?answer.load, %refusal, "track grid unavailable");
                self.grid = None;
                if self.synced() {
                    self.mode = SyncMode::Unsyncable;
                    self.cue = None;
                    self.alignment = None;
                    self.resume_wait(out);
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
            self.retime(trajectory, at, out);
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
