use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_warp::{
    BeatGridQuery, BeatGridSnapshot, BeatsPerMinute, MapAxis, MapPoint, MapPosition, MapRegion,
    MeterFacts, SessionAnchor, SessionBeat,
};

use super::{
    descent::{Parent, Takeover},
    state::{GroupState, Withdrawal, validate_successor},
    transaction::take_operation,
};
use crate::{
    ParentFact, ParentGridUpdate, ParentWithdrawal, SyncAdmission, SyncCapability, SyncError,
    SyncGroup, SyncIntent, SyncMode, SyncOperationId, consts,
};

/// The beat timeline one group follows, owned together with its mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Timeline {
    /// No musical timeline; an external owner may publish the grid.
    Off,
    /// The group's own tempo and phase, once a first tempo established them.
    Local(Option<LocalTimeline>),
    /// The parent's accepted segment, recorded on the group state.
    Host,
}

/// A local tempo trajectory together with the meter it carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LocalTimeline {
    anchor: SessionAnchor,
    meter: Option<MeterFacts>,
}

/// A mode operation that needs grid coverage not yet published, reported
/// until a later operation supersedes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Blocked {
    pub(super) operation: SyncOperationId,
    pub(super) required: MapRegion,
}

/// A mode operation evaluated against frozen state, before any mutation.
enum ModeEffect {
    /// The group moves onto `grid` from `at` on; `release` hands every
    /// sounding member off under that transport when the timeline ends.
    Changed {
        timeline: Timeline,
        grid: BeatGridSnapshot,
        descent: Option<ParentFact>,
        at: SessionFrame,
        release: Option<TransportRevision>,
    },
    Unchanged,
    Deferred {
        required: MapRegion,
    },
}

/// What a mode operation reports once its staged state is validated.
#[derive(Clone, Copy)]
enum Committed {
    Changed(Timeline),
    Unchanged,
    Deferred(MapRegion),
}

impl Timeline {
    pub(super) const fn without_geometry(mode: SyncMode) -> Self {
        match mode {
            SyncMode::Off => Self::Off,
            SyncMode::LocalSync => Self::Local(None),
            SyncMode::HostSync => Self::Host,
        }
    }

    pub(super) const fn mode(self) -> SyncMode {
        match self {
            Self::Off => SyncMode::Off,
            Self::Local(_) => SyncMode::LocalSync,
            Self::Host => SyncMode::HostSync,
        }
    }

    pub(super) fn tempo(self, parent: Option<&ParentGridUpdate>) -> Option<BeatsPerMinute> {
        let beats_per_second = match self {
            Self::Off | Self::Local(None) => return None,
            Self::Local(Some(local)) => local.anchor.target_beats_per_second(),
            Self::Host => parent?.anchor().target_beats_per_second(),
        };
        BeatsPerMinute::try_from(beats_per_second * consts::SECONDS_PER_MINUTE).ok()
    }

    /// The same mode on a new physical axis, where no frame of the old one
    /// is meaningful.
    pub(super) const fn on_new_axis(self) -> Self {
        match self {
            Self::Local(_) => Self::Local(None),
            Self::Off | Self::Host => self,
        }
    }
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Applies one mode intent addressed to this group.
    pub(super) fn transact_intent(
        &mut self,
        intent: SyncIntent,
        activation: SessionFrame,
        transport: TransportRevision,
    ) -> Result<SyncAdmission, SyncError> {
        self.reserve_operation()?;
        let effect = match intent {
            SyncIntent::Enable | SyncIntent::AlignNow => self.follow_parent(activation)?,
            SyncIntent::Disable => self.latch(activation)?,
            SyncIntent::Free => self.leave_timeline(activation, transport)?,
        };
        self.commit(effect)
    }

    /// Commits a tempo on a group that owns its timeline.
    pub(super) fn transact_tempo(
        &mut self,
        tempo: BeatsPerMinute,
        commit: SessionFrame,
        smoothing: f64,
    ) -> Result<SyncAdmission, SyncError> {
        let beats_per_second = f64::from(tempo) / consts::SECONDS_PER_MINUTE;
        let local = match self.timeline {
            Timeline::Off => {
                return Err(SyncError::CapabilityUnavailable {
                    capability: SyncCapability::Transport,
                });
            }
            Timeline::Host => {
                return Err(SyncError::TempoInherited {
                    owner: self.grid.id(),
                });
            }
            Timeline::Local(Some(local)) => LocalTimeline {
                anchor: local.anchor.retarget(commit, beats_per_second, smoothing)?,
                meter: local.meter,
            },
            Timeline::Local(None) => {
                let MapAxis::Session(axis) = self.grid.axis() else {
                    return Err(SyncError::InvalidGroupGridState {
                        state: self.grid.state(),
                    });
                };
                LocalTimeline {
                    anchor: SessionAnchor::new(
                        commit,
                        SessionBeat::default(),
                        beats_per_second,
                        axis.sample_rate(),
                    )?,
                    meter: None,
                }
            }
        };
        self.reserve_operation()?;
        let effect = self.local_effect(local, commit)?;
        self.commit(effect)
    }

    fn follow_parent(&self, at: SessionFrame) -> Result<ModeEffect, SyncError> {
        if matches!(self.timeline, Timeline::Host) {
            return Ok(ModeEffect::Unchanged);
        }
        let (grid, descent) = if let Some(parent) = self.parent.and_then(Parent::segment) {
            self.derived_grid(parent.epoch(), parent.anchor(), parent.meter())?
        } else {
            let grid = self.withdrawn_grid()?;
            let descent = ParentWithdrawal::new(grid.stamp(), at, None);
            (grid, ParentFact::Withdrawn(descent))
        };
        validate_successor(&self.grid, &grid, Withdrawal::Allowed)?;
        Ok(ModeEffect::Changed {
            timeline: Timeline::Host,
            grid,
            descent: Some(descent),
            at,
            release: None,
        })
    }

    /// Fixes the beat and tempo actually playing at `activation` as the
    /// group's own timeline, including mid-way through a tempo approach.
    fn latch(&self, activation: SessionFrame) -> Result<ModeEffect, SyncError> {
        if matches!(self.timeline, Timeline::Local(_)) {
            return Ok(ModeEffect::Unchanged);
        }
        let Some(local) = latch_at(&self.grid, activation)? else {
            return Ok(ModeEffect::Deferred {
                required: MapRegion::point(MapPosition::Session(activation)),
            });
        };
        self.local_effect(local, activation)
    }

    fn leave_timeline(
        &self,
        at: SessionFrame,
        transport: TransportRevision,
    ) -> Result<ModeEffect, SyncError> {
        if matches!(self.timeline, Timeline::Off) {
            return Ok(ModeEffect::Unchanged);
        }
        let grid = self.withdrawn_grid()?;
        validate_successor(&self.grid, &grid, Withdrawal::Allowed)?;
        let descent = ParentWithdrawal::new(grid.stamp(), at, Some(transport));
        Ok(ModeEffect::Changed {
            timeline: Timeline::Off,
            grid,
            descent: Some(ParentFact::Withdrawn(descent)),
            at,
            release: Some(transport),
        })
    }

    fn local_effect(
        &self,
        local: LocalTimeline,
        at: SessionFrame,
    ) -> Result<ModeEffect, SyncError> {
        let MapAxis::Session(axis) = self.grid.axis() else {
            return Err(SyncError::InvalidGroupGridState {
                state: self.grid.state(),
            });
        };
        let (grid, descent) = self.derived_grid(axis.epoch(), local.anchor, local.meter)?;
        validate_successor(&self.grid, &grid, Withdrawal::Refused)?;
        Ok(ModeEffect::Changed {
            timeline: Timeline::Local(Some(local)),
            grid,
            descent: Some(descent),
            at,
            release: None,
        })
    }

    /// The next grid revision following `anchor`, and the segment it hands
    /// every direct child group.
    pub(super) fn derived_grid(
        &self,
        epoch: SessionEpoch,
        anchor: SessionAnchor,
        meter: Option<MeterFacts>,
    ) -> Result<(BeatGridSnapshot, ParentFact), SyncError> {
        let grid =
            BeatGridSnapshot::session(self.grid.id(), self.next_revision()?, epoch, anchor, meter);
        let segment = ParentGridUpdate::new(grid.stamp(), epoch, anchor, meter);
        Ok((grid, ParentFact::Segment(segment)))
    }

    pub(super) fn withdrawn_grid(&self) -> Result<BeatGridSnapshot, SyncError> {
        Ok(BeatGridSnapshot::unavailable(
            self.grid.id(),
            self.next_revision()?,
            self.grid.axis(),
        ))
    }

    fn reserve_operation(&self) -> Result<SyncOperationId, SyncError> {
        self.next_operation
            .ok_or_else(|| SyncError::OperationIdExhausted {
                group_id: self.grid.id(),
            })
    }

    /// Stages and validates `effect` in full, then commits it.
    ///
    /// The operation itself takes the first identity; retargets and handoffs
    /// its staging mints take the ones after it.
    fn commit(&mut self, effect: ModeEffect) -> Result<SyncAdmission, SyncError> {
        let mut next_operation = self.next_operation;
        let operation = take_operation(self.grid.id(), &mut next_operation)?;
        let (effect, staged) = match effect {
            ModeEffect::Changed {
                timeline,
                grid,
                descent,
                at,
                release,
            } => {
                let takeover = Takeover {
                    commit: Some(at),
                    next_operation,
                };
                let mut staged = self.stage(grid, timeline, self.parent, descent, takeover)?;
                if let Some(transport) = release {
                    staged.pending = self.handoffs(&staged.grid, operation, transport, at)?;
                }
                (Committed::Changed(timeline), Some(staged))
            }
            ModeEffect::Unchanged => (Committed::Unchanged, None),
            ModeEffect::Deferred { required } => (Committed::Deferred(required), None),
        };
        let topology = self.topology_stamp();
        if staged.is_none() {
            self.next_operation = next_operation;
        }
        let transition = self.apply(staged);
        Ok(match effect {
            Committed::Changed(timeline) => {
                self.blocked = None;
                SyncAdmission::StateChanged {
                    operation,
                    topology,
                    mode: timeline.mode(),
                    grid: self.grid.stamp(),
                    transition,
                }
            }
            Committed::Unchanged => {
                self.blocked = None;
                SyncAdmission::Unchanged {
                    operation,
                    topology,
                }
            }
            Committed::Deferred(required) => {
                self.blocked = Some(Blocked {
                    operation,
                    required,
                });
                SyncAdmission::Deferred {
                    operation,
                    topology,
                    required,
                }
            }
        })
    }
}

/// Reads the beat, tempo, and meter actually playing at `frame`, or `None`
/// while the grid cannot answer there yet.
fn latch_at(
    grid: &BeatGridSnapshot,
    frame: SessionFrame,
) -> Result<Option<LocalTimeline>, SyncError> {
    let MapAxis::Session(axis) = grid.axis() else {
        return Ok(None);
    };
    let position = MapPoint::new(grid.stamp(), MapPosition::Session(frame));
    let (BeatGridQuery::Resolved(beat), BeatGridQuery::Resolved(tempo)) =
        (grid.beat_at(position), grid.tempo_at(position))
    else {
        return Ok(None);
    };
    let meter = match grid.meter_at(*beat.value()) {
        BeatGridQuery::Resolved(meter) => Some(MeterFacts::new(
            *meter.value(),
            meter.evidence(),
            meter.uncertainty(),
        )),
        _ => None,
    };
    let anchor = SessionAnchor::new(
        frame,
        SessionBeat::new(f64::from(*beat.value().value()))?,
        f64::from(*tempo.value()) / consts::SECONDS_PER_MINUTE,
        axis.sample_rate(),
    )?;
    Ok(Some(LocalTimeline { anchor, meter }))
}
