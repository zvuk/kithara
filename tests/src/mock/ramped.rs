use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

use kithara::{
    events::TrackId,
    platform::{
        sync::{Arc, Mutex},
        time::Duration,
    },
    play::{
        Bound, Outbox, PlayError, Player, PlayerConfig, PlayerFactory, PlayerImpl, ResourceLoad,
        Settled, Track, TrackCommand, TrackFactory, TrackReceipt, TrackSettings,
        TrackSettingsChange, TrackSnapshot,
    },
    signal::SessionFrame,
    warp::SpeedCurve,
};
use kithara_command::{Seq, When};
use kithara_render::bridge::DeckPart;

use crate::bufpool_ext::TestPools;

pub struct RampedFactory(pub NonZeroU64);

#[derive(Clone, Default)]
pub struct InjectedFactory(Arc<Mutex<BTreeMap<TrackId, ResourceLoad<TestPools>>>>);

impl InjectedFactory {
    pub fn insert(&self, id: TrackId, load: ResourceLoad<TestPools>) {
        assert!(
            self.0.lock().insert(id, load).is_none(),
            "one explicit fixture load per item"
        );
    }
}

impl TrackFactory<TestPools> for InjectedFactory {
    type Track = RampedTrack;

    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        Ok(RampedTrack {
            item: config.item,
            inner: PlayerFactory.track(config)?,
            frames: None,
            loads: Some(self.clone()),
        })
    }
}

pub struct RampedTrack {
    inner: PlayerImpl<TestPools>,
    item: TrackId,
    frames: Option<NonZeroU64>,
    loads: Option<InjectedFactory>,
}

impl TrackFactory<TestPools> for RampedFactory {
    type Track = RampedTrack;

    fn track(&self, config: PlayerConfig) -> Result<Self::Track, PlayError> {
        Ok(RampedTrack {
            item: config.item,
            inner: PlayerFactory.track(config)?,
            frames: Some(self.0),
            loads: None,
        })
    }
}

impl Player<TestPools> for RampedTrack {
    type Command = TrackCommand<TestPools>;
    type Snapshot = TrackSnapshot;

    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_, TestPools>,
    ) -> Result<Option<Seq>, PlayError> {
        let command = match (command, self.frames) {
            (
                TrackCommand::Configure(TrackSettingsChange::Speed(to), at)
                | TrackCommand::SetSpeed {
                    speed: SpeedCurve::Constant(to),
                    at,
                },
                Some(frames),
            ) => TrackCommand::SetSpeed {
                speed: SpeedCurve::Ramp { to, frames },
                at,
            },
            (TrackCommand::Load { item, position }, _) => {
                let item = match &self.loads {
                    Some(loads) => loads.0.lock().remove(&self.item).unwrap_or(item),
                    None => item,
                };
                TrackCommand::Load { item, position }
            }
            (command, _) => command,
        };
        self.inner.apply(command, out)
    }

    delegate::delegate! {
        to self.inner {
            fn entry(&self, bound: Bound) -> Option<SessionFrame>;
            fn settle(&mut self, receipt: TrackReceipt<'_, TestPools>, out: &mut Outbox<'_, TestPools>) -> Settled;
            fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_, TestPools>);
            fn snapshot(&self) -> Self::Snapshot;
        }
    }
}

impl Track<TestPools> for RampedTrack {
    delegate::delegate! {
        to self.inner {
            fn admit(&mut self, change: TrackSettingsChange, at: When<SessionFrame>, out: &Outbox<'_, TestPools>) -> Result<(), PlayError>;
            fn projected(&self) -> TrackSettings;
            fn planned(&self, at: SessionFrame, sample_rate: NonZeroU32) -> Result<(Duration, f32), PlayError>;
            fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError>;
            fn speed_receipt(&mut self) -> Option<Settled>;
            fn speed_applied(&mut self, seq: Seq) -> Option<bool>;
            fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>);
            fn cue(&mut self, position: Duration, speed: f32, out: &mut Outbox<'_, TestPools>) -> Result<Option<Seq>, PlayError>;
        }
    }
}
