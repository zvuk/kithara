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
        SeekBatch, Settled, Track, TrackCommand, TrackFactory, TrackReceipt, TrackSettings,
        TrackSettingsChange, TrackSnapshot,
    },
    signal::SessionFrame,
    sync::SpeedCurve,
};
use kithara_command::{Seq, When};
use kithara_render::bridge::DeckPart;

pub struct RampedFactory(pub NonZeroU64);

#[derive(Clone, Default)]
pub struct InjectedFactory(Arc<Mutex<BTreeMap<TrackId, ResourceLoad>>>);

impl InjectedFactory {
    pub fn insert(&self, id: TrackId, load: ResourceLoad) {
        assert!(
            self.0.lock().insert(id, load).is_none(),
            "one explicit fixture load per item"
        );
    }
}

impl TrackFactory for InjectedFactory {
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
    inner: PlayerImpl,
    item: TrackId,
    frames: Option<NonZeroU64>,
    loads: Option<InjectedFactory>,
}

impl TrackFactory for RampedFactory {
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

impl Player for RampedTrack {
    type Command = TrackCommand;
    type Snapshot = TrackSnapshot;

    fn apply(
        &mut self,
        command: Self::Command,
        out: &mut Outbox<'_>,
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
            fn settle(&mut self, receipt: TrackReceipt<'_>, out: &mut Outbox<'_>) -> Settled;
            fn tick(&mut self, now: SessionFrame, out: &mut Outbox<'_>);
            fn snapshot(&self) -> Self::Snapshot;
        }
    }
}

impl Track for RampedTrack {
    delegate::delegate! {
        to self.inner {
            fn admit(&mut self, change: TrackSettingsChange, at: When<SessionFrame>, out: &Outbox<'_>) -> Result<Option<usize>, PlayError>;
            fn projected(&self) -> TrackSettings;
            fn source_request(&self) -> Option<Seq>;
            fn source_closed(&self) -> bool;
            fn prepared_speed(&self) -> Option<&SpeedCurve>;
            fn prepare_entry(&mut self, at: SessionFrame, out: &mut Outbox<'_>) -> Result<bool, PlayError>;
            fn planned(&self, at: SessionFrame, sample_rate: NonZeroU32) -> Result<(Duration, f64), PlayError>;
            fn planned_law(&self, at: SessionFrame) -> Result<SpeedCurve, PlayError>;
            fn planned_end(&self, sample_rate: NonZeroU32) -> Result<Option<SessionFrame>, PlayError>;
            fn lane_receipt(&mut self) -> Option<Settled<kithara_render::LaneFrame>>;
            fn seek(&mut self, to: Duration, out: &mut Outbox<'_>) -> Result<Option<SeekBatch>, PlayError>;
            fn seek_applied(&mut self, seq: Seq) -> Option<bool>;
            fn finish_group(&mut self, result: Result<Seq, &mut Vec<DeckPart>>);
            fn cue(&mut self, position: Duration, speed: SpeedCurve, out: &mut Outbox<'_>) -> Result<Option<Seq>, PlayError>;
        }
    }
}
