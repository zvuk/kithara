use kithara_bufpool::HasPool;
use kithara_command::{Mailbox, Postbox, Seq, When};
use kithara_config::LiveConfig;
use kithara_events::TrackId;
use kithara_play::{
    DeckEqChange, DeckMixSettingsChange, EqBandConfig, GainDb, InterruptionKind, Outbox,
    OutputSnapshot, PlayError, Player, Position, Track, TrackCommand, TrackFactory, TrackSettings,
    TrackSettingsChange, TrackStatus as PlayingStatus,
};
use kithara_signal::{FaderValue, SessionFrame};

use super::{
    Queue, Transition,
    slots::{LoadState, Role},
    transition::TransitionRequest,
    types::{Placement, SeekPosition},
};
use crate::{
    ActionAtItemEnd, AdvanceReason, PlaybackOrder, QueueError, QueueEvent, QueueRepeatMode,
    QueueSettingsChange, RepeatMode, TrackSource, TrackStatus, loading::LoadReport,
};

pub(crate) type QueuePostbox<S> = Postbox<QueueCommand<S>, QueueError>;
pub(super) type QueueMailbox<S> = Mailbox<QueueCommand<S>, QueueError>;

/// Commands addressed to stable queue item identities, not list positions.
pub enum QueueCommand<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    Append {
        id: TrackId,
        source: TrackSource<S>,
        position: Position,
    },
    Insert {
        id: TrackId,
        source: TrackSource<S>,
        after: Option<TrackId>,
    },
    Remove(TrackId),
    RemoveAll,
    SetTracks(Vec<TrackSource<S>>),
    Select {
        id: TrackId,
        transition: Transition,
    },
    Next(Transition),
    Previous(Transition),
    Play {
        at: When<SessionFrame>,
    },
    Pause {
        at: When<SessionFrame>,
    },
    Seek {
        to: Position,
    },
    ConfigureTrack(TrackSettingsChange, When<SessionFrame>),
    ConfigureEffect {
        item: TrackId,
        lane: kithara_play::LaneId,
        target: kithara_play::EffectTarget,
        change: kithara_play::EffectChange,
        at: When<SessionFrame>,
    },
    ConfigureQueue(QueueSettingsChange, When<SessionFrame>),
    SetActionAtItemEnd(ActionAtItemEnd),
    SetPlaybackOrder(PlaybackOrder),
    SetRepeat(RepeatMode),
    SetVolume(f32),
    SetLevel(f32),
    SetMuted(bool),
    SetEqGain {
        band: usize,
        gain_db: f32,
    },
    SetEqLayout(Vec<EqBandConfig>),
    ResetEq,
    NotifyInterruption(InterruptionKind),
    Tick,
    Close,
    #[doc(hidden)]
    Load(LoadReport),
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) fn validate_command(&self, command: &QueueCommand<S>) -> Result<(), QueueError> {
        self.ensure_open()?;
        if let QueueCommand::Select { transition, .. } = command {
            transition
                .settings(self.config.settings.crossfade())
                .validate()
                .map_err(PlayError::from)?;
        }
        let id = match command {
            QueueCommand::Select { id, .. } | QueueCommand::Remove(id) => Some(*id),
            QueueCommand::Insert { after, .. } => *after,
            _ => None,
        };
        if let Some(id) = id {
            self.tracks
                .records()
                .iter()
                .find(|record| record.id == id)
                .ok_or(QueueError::UnknownTrackId(id))?;
        }
        Ok(())
    }

    pub(super) fn apply_command(
        &mut self,
        command: QueueCommand<S>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        self.validate_command(&command)?;
        match command {
            command @ (QueueCommand::Append { .. }
            | QueueCommand::Insert { .. }
            | QueueCommand::Remove(_)
            | QueueCommand::RemoveAll
            | QueueCommand::SetTracks(_)
            | QueueCommand::Load(_)
            | QueueCommand::Tick
            | QueueCommand::Close) => self.apply_items(command, output, out),
            command @ (QueueCommand::ConfigureQueue(..)
            | QueueCommand::SetActionAtItemEnd(_)
            | QueueCommand::SetPlaybackOrder(_)
            | QueueCommand::SetRepeat(_)
            | QueueCommand::SetVolume(_)
            | QueueCommand::SetLevel(_)
            | QueueCommand::SetMuted(_)
            | QueueCommand::SetEqGain { .. }
            | QueueCommand::SetEqLayout(_)
            | QueueCommand::ResetEq) => self.apply_queue_settings(command, out),
            QueueCommand::Select { id, transition } => self.request_transition(
                TransitionRequest {
                    id,
                    transition,
                    reason: AdvanceReason::UserSelect,
                    auto: false,
                    playing: true,
                },
                output,
                out,
            ),
            QueueCommand::Next(transition) => self.next_target(
                transition,
                AdvanceReason::UserNext,
                false,
                true,
                output,
                out,
            ),
            QueueCommand::Previous(transition) => {
                let ids = self.track_ids();
                self.navigation.prev(&ids).map_or(Ok(None), |id| {
                    self.request_transition(
                        TransitionRequest {
                            id,
                            transition,
                            reason: AdvanceReason::UserPrev,
                            auto: false,
                            playing: true,
                        },
                        output,
                        out,
                    )
                })
            }
            QueueCommand::Play { at } => self.play_at(at, output, out),
            QueueCommand::Pause { at } => self.pause_at(at, out),
            QueueCommand::Seek { to } => self.seek_to(to, out),
            QueueCommand::ConfigureTrack(change, at) => self.configure_tracks(change, at, out),
            QueueCommand::ConfigureEffect {
                item,
                lane,
                target,
                change,
                at,
            } => {
                let index = self
                    .active
                    .position(|active| {
                        let snapshot = active.track.snapshot();
                        let snapshot = snapshot.as_ref();
                        snapshot.item == item
                            && snapshot.effects.iter().any(|effect| effect.lane() == lane)
                    })
                    .ok_or(PlayError::NotReady)?;
                self.active
                    .get_mut(index)
                    .ok_or(PlayError::NotReady)?
                    .track
                    .apply(
                        TrackCommand::ConfigureEffect {
                            lane,
                            target,
                            change,
                            at,
                        },
                        out,
                    )
                    .map_err(Into::into)
            }
            QueueCommand::NotifyInterruption(kind) => {
                self.deck.suspended = out.notify_interruption(kind)?;
                Ok(None)
            }
        }
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    fn apply_items(
        &mut self,
        command: QueueCommand<S>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        match command {
            QueueCommand::Append {
                id,
                source,
                position,
            } => {
                self.insert_entry(id, source, position, Placement::Append);
                self.arm_initial_load(output, out);
                Ok(None)
            }
            QueueCommand::Insert { id, source, after } => {
                self.insert_after(id, source, after, output, out)
            }
            QueueCommand::Remove(id) => self.remove_entry(id, output, out),
            QueueCommand::RemoveAll => self.clear_entries(out),
            QueueCommand::SetTracks(sources) => {
                self.clear_entries(out)?;
                for source in sources {
                    let id = TrackId::allocate();
                    self.insert_entry(id, source, Position::ZERO, Placement::Append);
                }
                self.arm_initial_load(output, out);
                Ok(None)
            }
            QueueCommand::Load(report) => {
                self.tracks.apply_report(report);
                Ok(None)
            }
            QueueCommand::Tick => {
                if let Some((now, _)) = self.clock {
                    Player::tick(self, now, out);
                }
                Ok(None)
            }
            QueueCommand::Close => self.close_tracks(out).map_err(Into::into),
            _ => unreachable!("item dispatch accepts only item lifecycle commands"),
        }
    }

    fn insert_after(
        &mut self,
        id: TrackId,
        source: TrackSource<S>,
        after: Option<TrackId>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let index = match after {
            Some(after) => {
                self.tracks
                    .records()
                    .iter()
                    .position(|record| record.id == after)
                    .ok_or(QueueError::UnknownTrackId(after))?
                    + 1
            }
            None => 0,
        };
        self.insert_entry(id, source, Position::ZERO, Placement::At(index));
        if after == self.current && self.current.is_some() {
            self.cancel_auto(out)?;
        }
        self.arm_initial_load(output, out);
        Ok(None)
    }

    fn apply_queue_settings(
        &mut self,
        command: QueueCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        match command {
            QueueCommand::SetActionAtItemEnd(action) => {
                self.config.action_at_item_end = action;
                self.announce(QueueEvent::ActionAtItemEndChanged { action });
                self.cancel_auto(out)?;
                Ok(None)
            }
            QueueCommand::SetPlaybackOrder(order) => {
                let ids = self.track_ids();
                self.navigation
                    .set_playback_order(order, &ids, self.current);
                self.config.playback_order = order;
                self.announce(QueueEvent::PlaybackOrderChanged { order });
                self.cancel_auto(out)?;
                Ok(None)
            }
            QueueCommand::SetRepeat(mode) => self.set_repeat_mode(mode, out),
            QueueCommand::SetVolume(volume) => out
                .mix(
                    When::Next,
                    DeckMixSettingsChange::Volume(FaderValue::from(volume)),
                )
                .map(Some)
                .map_err(Into::into),
            QueueCommand::SetLevel(level) => out
                .mix(When::Next, DeckMixSettingsChange::Level(level))
                .map(Some)
                .map_err(Into::into),
            QueueCommand::SetMuted(muted) => out
                .mix(When::Next, DeckMixSettingsChange::Muted(muted))
                .map(Some)
                .map_err(Into::into),
            QueueCommand::SetEqGain { band, gain_db } => {
                self.set_eq_gain(band, gain_db, out).map_err(Into::into)
            }
            QueueCommand::SetEqLayout(bands) => self.set_eq_layout(&bands, out).map_err(Into::into),
            QueueCommand::ResetEq => out
                .eq((0..self.deck.mixer.eq.bands())
                    .map(|band| DeckEqChange::Gain {
                        band,
                        gain: GainDb::default(),
                    })
                    .collect())
                .map_err(Into::into),
            QueueCommand::ConfigureQueue(change, at) => self.configure_queue(change, at, out),
            _ => unreachable!("settings dispatch accepts only queue settings commands"),
        }
    }

    fn set_repeat_mode(
        &mut self,
        mode: RepeatMode,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        self.navigation.set_repeat(mode);
        let mode = match mode {
            RepeatMode::Off => QueueRepeatMode::Off,
            RepeatMode::One => QueueRepeatMode::One,
            RepeatMode::All => QueueRepeatMode::All,
        };
        self.announce(QueueEvent::RepeatModeChanged { mode });
        self.cancel_auto(out)?;
        Ok(None)
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    fn pause_at(
        &mut self,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        if self.active_current_index().is_none() && self.target.is_some() {
            self.withdraw_transition(out)?;
            self.target
                .as_mut()
                .ok_or(PlayError::NotReady)?
                .set_playing(false);
            return self.transition_loaded(out).map_err(Into::into);
        }
        if self.pending_handover.is_some()
            && let Some(target) = self.target.as_mut()
            && self.current == Some(target.to)
        {
            target.set_playing(false);
        }
        self.cancel_auto(out)?;
        self.transport(TrackCommand::Pause { at }, out)
            .map_err(Into::into)
    }

    fn play_at(
        &mut self,
        at: When<SessionFrame>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let requested = self
            .target
            .filter(|target| !target.auto)
            .map(|target| (target.to, target.transition))
            .or_else(|| self.current.map(|id| (id, Transition::None)));
        if let Some((id, transition)) = requested
            && self
                .tracks
                .records()
                .iter()
                .any(|record| record.id == id && matches!(record.status, TrackStatus::Failed(_)))
        {
            return self.request_transition(
                TransitionRequest {
                    id,
                    transition,
                    reason: AdvanceReason::UserSelect,
                    auto: false,
                    playing: true,
                },
                output,
                out,
            );
        }
        if self.active_current_index().is_some() {
            if self.pending_handover.is_some()
                && let Some(target) = self.target.as_mut()
                && self.current == Some(target.to)
            {
                target.set_playing(true);
            }
            let sent = self.transport(TrackCommand::Play { at }, out)?;
            if let Some(id) = self.current {
                self.tracks.set_status(id, TrackStatus::Consumed);
            }
            Ok(sent)
        } else if self.target.is_some() {
            self.target
                .as_mut()
                .ok_or(PlayError::NotReady)?
                .set_playing(true);
            self.transition_loaded(out).map_err(Into::into)
        } else {
            self.next_target(
                Transition::None,
                AdvanceReason::InitialLoad,
                false,
                true,
                output,
                out,
            )
        }
    }

    fn configure_queue(
        &mut self,
        change: QueueSettingsChange,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        if matches!(at, When::At(_)) {
            return Err(PlayError::Untimed.into());
        }
        let change = match change {
            QueueSettingsChange::Crossfade(settings) => {
                QueueSettingsChange::Crossfade(settings.validate().map_err(PlayError::from)?)
            }
            change @ QueueSettingsChange::Gapless(_) => change,
        };
        self.config.settings.apply_change(change);
        if let QueueSettingsChange::Crossfade(settings) = change {
            self.announce(QueueEvent::CrossfadeSettingsChanged { settings });
        }
        self.withdraw_transition(out)?;
        Ok(None)
    }

    pub(super) fn transport(
        &mut self,
        command: TrackCommand<S>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let index = self.active_current_index().ok_or(PlayError::NoActiveSlot)?;
        self.active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(command, out)
    }

    fn configure_tracks(
        &mut self,
        change: TrackSettingsChange,
        at: When<SessionFrame>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let change = TrackSettings::check(change)?;
        let receivers: Vec<_> = self
            .active
            .indices(|active| active.role != Role::Leaving)
            .into_iter()
            .map(|index| {
                let sounding = self.active.get(index).is_some_and(|active| {
                    matches!(
                        active.track.snapshot().as_ref().status,
                        PlayingStatus::Playing { .. }
                    )
                });
                (index, if sounding { at } else { When::Next })
            })
            .collect();
        if matches!(at, When::At(_)) && !receivers.iter().any(|(_, when)| *when == at) {
            return Err(PlayError::Untimed.into());
        }
        let current = self.active_current_index();
        let (deck_cost, moves_current) =
            self.admit_track_settings(change, &receivers, current, out)?;
        let withdraw = matches!(change, TrackSettingsChange::Speed(_))
            && moves_current
            && self.target.is_some_and(|target| {
                target.auto
                    && target.stale.is_none()
                    && target.repeat.is_none()
                    && self
                        .pending_handover
                        .is_some_and(|pending| pending.item == target.to)
                    && self
                        .incoming_index(target.to)
                        .and_then(|index| self.active.get(index))
                        .is_some_and(|active| matches!(active.role, Role::Incoming))
            });
        if deck_cost + usize::from(withdraw) > out.deck_available() {
            return Err(PlayError::Full("deck").into());
        }
        let moved = self.apply_track_settings(change, receivers, current, out)?;
        self.config.track.apply_change(change);
        if withdraw && moved {
            self.withdraw_auto(out)?;
        }
        Ok(None)
    }

    fn admit_track_settings(
        &mut self,
        change: TrackSettingsChange,
        receivers: &[(usize, When<SessionFrame>)],
        current: Option<usize>,
        out: &Outbox<'_, S>,
    ) -> Result<(usize, bool), PlayError> {
        let mut deck_cost = 0;
        let mut moves_current = false;
        for &(index, when) in receivers {
            let cost = self
                .active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .admit(change, when, out)?;
            deck_cost += cost.unwrap_or(0);
            moves_current |= Some(index) == current && cost.is_some();
        }
        for parked in self
            .active
            .parked_iter_mut()
            .filter(|parked| parked.track.snapshot().as_ref().status != PlayingStatus::Released)
        {
            deck_cost += parked.track.admit(change, When::Next, out)?.unwrap_or(0);
        }
        Ok((deck_cost, moves_current))
    }

    fn apply_track_settings(
        &mut self,
        change: TrackSettingsChange,
        receivers: Vec<(usize, When<SessionFrame>)>,
        current: Option<usize>,
        out: &mut Outbox<'_, S>,
    ) -> Result<bool, PlayError> {
        let mut moved = false;
        for (index, when) in receivers {
            let sent = self
                .active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .apply(TrackCommand::Configure(change, when), out)?;
            moved |= Some(index) == current && sent.is_some();
        }
        for parked in self
            .active
            .parked_iter_mut()
            .filter(|parked| parked.track.snapshot().as_ref().status != PlayingStatus::Released)
        {
            parked
                .track
                .apply(TrackCommand::Configure(change, When::Next), out)?;
        }
        Ok(moved)
    }

    fn set_eq_gain(
        &self,
        band: usize,
        gain_db: f32,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        let bands = self.deck.mixer.eq.bands();
        if band >= bands {
            return Err(PlayError::EqBandOutOfRange { band, bands });
        }
        out.eq(vec![DeckEqChange::Gain {
            band,
            gain: GainDb::from(gain_db),
        }])
    }

    fn set_eq_layout(
        &self,
        bands: &[EqBandConfig],
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        if bands.len() > self.config.mixer.eq_bands() {
            return Err(PlayError::InvalidConfiguration {
                reason: "EQ layout exceeds the deck's configured band capacity".into(),
            });
        }
        let prep = self.config.prep.as_ref().ok_or(PlayError::NotReady)?;
        out.eq_layout(prep.worker.pools(), bands, self.config.mixer.sample_rate())
    }
}

impl<S, F> Queue<S, F>
where
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    F: TrackFactory<S>,
{
    pub(super) fn seek_to(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let index = self.active_current_index().or_else(|| {
            self.target
                .and_then(|target| self.incoming_index(target.to))
        });
        let Some(index) = index else {
            self.held_position = Some(SeekPosition::Held(to));
            return Ok(None);
        };
        let active = self.active.get_mut(index).ok_or(PlayError::NoActiveSlot)?;
        if matches!(active.load, Some(LoadState::Opening(_))) {
            self.held_position = Some(SeekPosition::Held(to));
            return Ok(None);
        }
        let item = active.item;
        let sent = active.track.apply(TrackCommand::Seek { to }, out)?;
        self.withdraw_auto(out)?;
        self.held_position = sent.map(|seq| SeekPosition::Pending {
            item,
            seq,
            to,
            segment: None,
        });
        Ok(sent)
    }
}

pub(super) fn play_error(error: QueueError) -> PlayError {
    match error {
        QueueError::Play(error) => error,
        QueueError::UnknownTrackId(item) => PlayError::ItemConsumed { item },
        QueueError::NotReady(_) => PlayError::NotReady,
        error => PlayError::ItemFailed {
            reason: error.to_string(),
        },
    }
}
