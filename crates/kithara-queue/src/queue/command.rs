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

use super::{Queue, Transition, slots::Role, transition::TransitionRequest, types::Placement};
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
            let record = self
                .tracks
                .records()
                .iter()
                .find(|record| record.id == id)
                .ok_or(QueueError::UnknownTrackId(id))?;
            if matches!(command, QueueCommand::Select { .. })
                && matches!(record.status, TrackStatus::Failed(_))
            {
                return Err(QueueError::NotReady(id));
            }
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
            QueueCommand::Append { id, source } => {
                self.insert_entry(id, source, Placement::Append);
                self.arm_initial_load(output, out);
                Ok(None)
            }
            QueueCommand::Insert { id, source, after } => {
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
                self.insert_entry(id, source, Placement::At(index));
                self.arm_initial_load(output, out);
                Ok(None)
            }
            QueueCommand::Remove(id) => self.remove_entry(id, output, out),
            QueueCommand::RemoveAll => self.clear_entries(out),
            QueueCommand::SetTracks(sources) => {
                self.clear_entries(out)?;
                for source in sources {
                    self.insert_entry(TrackId::allocate(), source, Placement::Append);
                }
                self.arm_initial_load(output, out);
                Ok(None)
            }
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
            QueueCommand::Pause { at } => {
                if self.active_current_index().is_none() && self.target.is_some() {
                    self.withdraw_transition(out)?;
                    self.target.as_mut().ok_or(PlayError::NotReady)?.playing = false;
                    return self.transition_loaded(out).map_err(Into::into);
                }
                self.cancel_auto(out)?;
                self.transport(TrackCommand::Pause { at }, out)
                    .map_err(Into::into)
            }
            QueueCommand::Seek { to } => self.seek_to(to, out),
            QueueCommand::ConfigureTrack(change, at) => self.configure_tracks(change, at, out),
            QueueCommand::ConfigureQueue(change, at) => self.configure_queue(change, at, out),
            QueueCommand::SetActionAtItemEnd(action) => {
                self.config.action_at_item_end = action;
                self.announce(QueueEvent::ActionAtItemEndChanged { action });
                self.cancel_auto(out)?;
                Ok(None)
            }
            QueueCommand::SetPlaybackOrder(order) => {
                let ids = self.track_ids();
                self.navigation.set_playback_order(order, &ids);
                self.config.playback_order = order;
                self.announce(QueueEvent::PlaybackOrderChanged { order });
                self.cancel_auto(out)?;
                Ok(None)
            }
            QueueCommand::SetRepeat(mode) => {
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
            QueueCommand::NotifyInterruption(kind) => {
                self.deck.suspended = out.notify_interruption(kind)?;
                Ok(None)
            }
        }
    }

    fn play_at(
        &mut self,
        at: When<SessionFrame>,
        output: Option<&OutputSnapshot>,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        if self.active_current_index().is_some() {
            self.transport(TrackCommand::Play { at }, out)
                .map_err(Into::into)
        } else if self.target.is_some() {
            self.target.as_mut().ok_or(PlayError::NotReady)?.playing = true;
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

    fn seek_to(
        &mut self,
        to: Position,
        out: &mut Outbox<'_, S>,
    ) -> Result<Option<Seq>, QueueError> {
        let index = self.active_current_index().or_else(|| {
            self.target
                .and_then(|target| self.incoming_index(target.to))
        });
        let Some(index) = index else {
            self.held_position = Some(to);
            return Ok(None);
        };
        let sent = self
            .active
            .get_mut(index)
            .ok_or(PlayError::NoActiveSlot)?
            .track
            .apply(TrackCommand::Seek { to }, out)?;
        self.withdraw_auto(out)?;
        Ok(sent)
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
        for &(index, when) in &receivers {
            self.active
                .get_mut(index)
                .ok_or(PlayError::NoActiveSlot)?
                .track
                .admit(change, when, out)?;
        }
        for parked in self
            .active
            .parked_iter_mut()
            .filter(|parked| parked.track.snapshot().as_ref().status != PlayingStatus::Released)
        {
            parked.track.admit(change, When::Next, out)?;
        }
        let withdraw = matches!(change, TrackSettingsChange::Speed(_))
            && self.target.is_some_and(|target| {
                target.auto
                    && target.stale.is_none()
                    && target.repeat.is_none()
                    && self
                        .incoming_index(target.to)
                        .and_then(|index| self.active.get(index))
                        .is_some_and(|active| {
                            matches!(active.role, Role::Incoming { batch: Some(_) })
                        })
            });
        if withdraw && out.deck_available() == 0 {
            return Err(PlayError::Full("deck").into());
        }
        let current = self.active_current_index();
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
        self.config.track.apply_change(change);
        if withdraw && moved {
            self.withdraw_auto(out)?;
        }
        Ok(None)
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
