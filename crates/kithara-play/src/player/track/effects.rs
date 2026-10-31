use arc_swap::ArcSwap;
use kithara_command::{Live, Outcome, Receipt, Seq, When};
use kithara_config::{ConfigOwner, LiveConfig};
use kithara_effects::{
    EffectChange, EffectConfig, EffectTarget,
    eq::{EqBandConfig, EqBandConfigChange},
};
use kithara_platform::sync::Arc;
use kithara_render::{LaneCommand, LaneFrame, LaneId, LaneProtocol, bridge::EffectProgram};
use kithara_signal::SessionFrame;

use super::core::PlayerImpl;
use crate::{Outbox, PlayError};

/// Read-only settings published by the track after executor receipts.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EffectConfigView<C>(Arc<ArcSwap<C>>);

impl<C: Copy> EffectConfigView<C> {
    fn new(config: C) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(config)))
    }

    /// Last configuration confirmed by the effect executor.
    #[must_use]
    pub fn config(&self) -> C {
        **self.0.load()
    }
}

impl<C: LiveConfig> EffectConfigView<C> {
    fn settle(&self, live: &mut Live<C, LaneProtocol>, receipt: &Receipt<LaneProtocol>) {
        if live.settle(receipt).is_some() && matches!(receipt.outcome(), Outcome::Applied { .. }) {
            self.0.store(Arc::new(*live.config()));
        }
    }
}

/// Settings and source identity of one installed built-in effect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum EffectSnapshot {
    Equalizer {
        lane: LaneId,
        bands: Arc<[EffectConfigView<EqBandConfig>]>,
    },
}

impl EffectSnapshot {
    /// Identity of the loaded source that owns this effect state.
    #[must_use]
    pub const fn lane(&self) -> LaneId {
        match self {
            Self::Equalizer { lane, .. } => *lane,
        }
    }
}

enum EffectState {
    Equalizer {
        bands: Vec<Live<EqBandConfig, LaneProtocol>>,
        views: Arc<[EffectConfigView<EqBandConfig>]>,
    },
}

#[derive(Default)]
pub(super) struct TrackEffects(Vec<EffectState>);

impl TrackEffects {
    pub(super) fn new(configs: &[EffectConfig]) -> Result<Self, PlayError> {
        let mut states: Vec<EffectState> = Vec::with_capacity(configs.len());
        for config in configs {
            let EffectConfig::Equalizer(bands) = config else {
                return Err(PlayError::InvalidConfiguration {
                    reason: "unsupported effect construction".into(),
                });
            };
            let views = bands.iter().copied().map(EffectConfigView::new).collect();
            let bands = bands
                .iter()
                .copied()
                .map(|band| match Live::new(band) {
                    Ok(live) => live,
                    Err(never) => match never {},
                })
                .collect();
            states.push(EffectState::Equalizer { bands, views });
        }
        Ok(Self(states))
    }

    pub(super) fn snapshot(&self, lane: Option<LaneId>) -> Vec<EffectSnapshot> {
        let Some(lane) = lane else { return Vec::new() };
        self.0
            .iter()
            .map(|state| match state {
                EffectState::Equalizer { views, .. } => EffectSnapshot::Equalizer {
                    lane,
                    bands: Arc::clone(views),
                },
            })
            .collect()
    }

    fn admit(&self, target: EffectTarget, change: EffectChange) -> Result<(), PlayError> {
        match (self.0.get(target.effect()), target, change) {
            (
                Some(EffectState::Equalizer { bands, .. }),
                EffectTarget::EqBand { band, .. },
                EffectChange::EqBand(_),
            ) if bands.get(band).is_some() => Ok(()),
            _ => Err(PlayError::InvalidConfiguration {
                reason: format!("effect target {target:?} is not installed"),
            }),
        }
    }

    fn track(
        &mut self,
        target: EffectTarget,
        change: EffectChange,
        seq: Seq,
        when: When<LaneFrame>,
    ) -> Result<(), PlayError> {
        match (self.0.get_mut(target.effect()), target, change) {
            (
                Some(EffectState::Equalizer { bands, .. }),
                EffectTarget::EqBand { band, .. },
                EffectChange::EqBand(change),
            ) => {
                let live = bands
                    .get_mut(band)
                    .ok_or_else(|| PlayError::InvalidConfiguration {
                        reason: format!("effect target {target:?} is not installed"),
                    })?;
                live.track(seq, when, change);
            }
            _ => {
                return Err(PlayError::InvalidConfiguration {
                    reason: format!("effect target {target:?} is not installed"),
                });
            }
        }
        Ok(())
    }

    pub(super) fn settle(&mut self, receipt: &Receipt<LaneProtocol>) {
        for state in &mut self.0 {
            let EffectState::Equalizer { bands, views } = state;
            for (live, view) in bands.iter_mut().zip(views.iter()) {
                view.settle(live, receipt);
            }
        }
        if matches!(receipt.outcome(), Outcome::Applied { .. }) {
            for command in &receipt.batch().commands {
                if let LaneCommand::EffectProgram {
                    program,
                    applied: Some(EffectChange::EqBand(EqBandConfigChange::GainDb(gain))),
                } = command
                {
                    let EffectTarget::EqBand { effect, band } = program.parameter().target() else {
                        continue;
                    };
                    if let Some(EffectState::Equalizer { bands, views }) = self.0.get_mut(effect)
                        && let (Some(live), Some(view)) = (bands.get_mut(band), views.get(band))
                    {
                        match live.apply(EqBandConfigChange::GainDb(*gain)) {
                            Ok(()) => view.0.store(Arc::new(*live.config())),
                            Err(never) => match never {},
                        }
                    }
                }
            }
        }
    }
}

impl<S> PlayerImpl<S> {
    pub(super) fn effect_program(
        &mut self,
        lane: LaneId,
        program: EffectProgram,
        out: &Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.effect_ready(lane)?;
        self.effects.admit(
            program.parameter().target(),
            EffectChange::EqBand(EqBandConfigChange::GainDb(crate::GainDb::DEFAULT)),
        )?;
        let when = self.lane_when(When::At(program.start()), out)?;
        self.send_lane(
            LaneCommand::EffectProgram {
                program,
                applied: None,
            },
            when,
        )
        .map(Some)
    }

    fn effect_ready(&self, lane: LaneId) -> Result<(), PlayError> {
        if self.lane_id != Some(lane)
            || self.lane.is_none()
            || self.releasing
            || matches!(
                self.status,
                super::state::TrackStatus::Released | super::state::TrackStatus::Failed { .. }
            )
        {
            return Err(PlayError::NotReady);
        }
        Ok(())
    }

    pub(super) fn configure_effect(
        &mut self,
        lane: LaneId,
        target: EffectTarget,
        change: EffectChange,
        at: When<SessionFrame>,
        out: &Outbox<'_, S>,
    ) -> Result<Option<Seq>, PlayError> {
        self.effect_ready(lane)?;
        self.effects.admit(target, change)?;
        let when = self.lane_when(at, out)?;
        let seq = self.send_lane(LaneCommand::ConfigureEffect { target, change }, when)?;
        self.effects.track(target, change, seq, when)?;
        Ok(Some(seq))
    }
}
