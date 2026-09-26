use kithara_signal::{SessionEpoch, SessionFrame, TransportRevision};
use kithara_warp::{
    BeatGridQuery, BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatsPerMinute, MapAxis,
    MapPoint, MapPosition, MapRegion, MeterFacts, SessionAnchor, SessionBeat, WarpPlan,
};

use super::{
    descent::{Parent, Takeover},
    preparation::SyncEntry,
    state::{GroupState, Withdrawal, validate_successor},
    transaction::take_operation,
};
use crate::{
    AlignmentSource, LoadGeneration, ParentFact, ParentGridUpdate, ParentWithdrawal, SyncAdmission,
    SyncCapability, SyncError, SyncGroup, SyncIntent, SyncMode, SyncOperationId,
};

const SECONDS_PER_MINUTE: f64 = 60.0;

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

/// The timeline a leaf deck still sounds through before its first Host entry
/// is presented. An already-Host entry has no prior mode to restore.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PriorTimeline {
    Off(BeatGridStamp),
    Local(Option<LocalTimeline>, BeatGridStamp),
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
        descent: Box<ParentFact>,
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
    const fn prior(self, grid: BeatGridStamp) -> Option<PriorTimeline> {
        match self {
            Self::Off => Some(PriorTimeline::Off(grid)),
            Self::Local(local) => Some(PriorTimeline::Local(local, grid)),
            Self::Host => None,
        }
    }

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
        BeatsPerMinute::try_from(beats_per_second * SECONDS_PER_MINUTE).ok()
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

impl PriorTimeline {
    pub(super) const fn timeline(self) -> Timeline {
        match self {
            Self::Off(_) => Timeline::Off,
            Self::Local(local, _) => Timeline::Local(local),
        }
    }

    pub(super) const fn grid(self) -> BeatGridStamp {
        match self {
            Self::Off(grid) | Self::Local(_, grid) => grid,
        }
    }
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Rebuilds the still-sounding timeline at a new monotonic grid revision
    /// when the executor rejects an entry before its audio claim.
    pub(super) fn restored_entry_grid(
        &self,
        prior: PriorTimeline,
    ) -> Result<BeatGridSnapshot, SyncError> {
        let (grid, withdrawal) = match prior {
            PriorTimeline::Off(_) | PriorTimeline::Local(None, _) => {
                (self.withdrawn_grid()?, Withdrawal::Allowed)
            }
            PriorTimeline::Local(Some(local), _) => {
                let MapAxis::Session(axis) = self.grid.axis() else {
                    return Err(SyncError::InvalidGroupGridState {
                        state: self.grid.state(),
                    });
                };
                (
                    self.derived_grid(axis.epoch(), local.anchor, local.meter, None, None)?
                        .0,
                    Withdrawal::Refused,
                )
            }
        };
        validate_successor(&self.grid, &grid, withdrawal)?;
        Ok(grid)
    }

    /// Applies one mode intent addressed to this group.
    pub(super) fn transact_intent(
        &mut self,
        intent: SyncIntent,
        load: LoadGeneration,
        transport: TransportRevision,
        source: AlignmentSource,
        activation: SessionFrame,
    ) -> Result<SyncAdmission, SyncError> {
        self.reserve_operation()?;
        let effect = match intent {
            SyncIntent::Enable | SyncIntent::AlignNow => self.follow_parent(activation)?,
            SyncIntent::Disable => self.latch(load, source, activation)?,
            SyncIntent::Free => self.leave_timeline(activation, transport)?,
        };
        let entry =
            matches!(intent, SyncIntent::Enable | SyncIntent::AlignNow).then_some(SyncEntry {
                load,
                transport,
                source,
                activation,
            });
        self.commit(effect, entry)
    }

    /// Commits a tempo on a group that owns its timeline.
    pub(super) fn transact_tempo(
        &mut self,
        tempo: BeatsPerMinute,
        commit: SessionFrame,
        smoothing: f64,
    ) -> Result<SyncAdmission, SyncError> {
        let beats_per_second = f64::from(tempo) / SECONDS_PER_MINUTE;
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
        self.commit(effect, None)
    }

    fn follow_parent(&self, at: SessionFrame) -> Result<ModeEffect, SyncError> {
        if matches!(self.timeline, Timeline::Host) {
            return Ok(ModeEffect::Unchanged);
        }
        let (grid, descent) = if let Some(parent) = self.parent.and_then(Parent::segment) {
            self.derived_grid(
                parent.epoch(),
                parent.anchor(),
                parent.meter(),
                parent.output_transport(),
                parent.execution_floor(),
            )?
        } else {
            let grid = self.withdrawn_grid()?;
            let descent = ParentWithdrawal::new(grid.stamp(), at, None);
            (grid, ParentFact::Withdrawn(descent))
        };
        validate_successor(&self.grid, &grid, Withdrawal::Allowed)?;
        Ok(ModeEffect::Changed {
            timeline: Timeline::Host,
            grid,
            descent: Box::new(descent),
            at,
            release: None,
        })
    }

    /// Fixes the beat and tempo actually playing at `activation` as the
    /// group's own timeline, including mid-way through a tempo approach.
    fn latch(
        &self,
        load: LoadGeneration,
        source: AlignmentSource,
        activation: SessionFrame,
    ) -> Result<ModeEffect, SyncError> {
        if matches!(self.timeline, Timeline::Local(_)) {
            return Ok(ModeEffect::Unchanged);
        }
        if let Some(armed) = self.pending.iter().find(|pending| pending.armed()) {
            return Err(SyncError::ArmedOperation {
                member_id: armed.member(),
                operation: armed.operation(),
            });
        }
        if let Some((_, prior)) = self.before_entry {
            return match prior {
                PriorTimeline::Local(Some(local), _) => self.local_effect(local, activation),
                PriorTimeline::Off(_) | PriorTimeline::Local(None, _) => {
                    let grid = self.withdrawn_grid()?;
                    validate_successor(&self.grid, &grid, Withdrawal::Allowed)?;
                    let descent = ParentWithdrawal::new(grid.stamp(), activation, None);
                    Ok(ModeEffect::Changed {
                        timeline: prior.timeline(),
                        grid,
                        descent: Box::new(ParentFact::Withdrawn(descent)),
                        at: activation,
                        release: None,
                    })
                }
            };
        }
        let local = match source {
            AlignmentSource::Audible { frontier, .. } if frontier.warp_map().is_some() => {
                let lane = self
                    .applied
                    .iter()
                    .find(|lane| Some(lane.map()) == frontier.warp_map())
                    .ok_or_else(|| self.unmatched_applied(frontier.warp_map()))?;
                let applied = lane.applied();
                if applied.stamp().load() != load {
                    return Err(SyncError::LoadMismatch {
                        member_id: lane.member(),
                        expected: applied.stamp().load(),
                        given: load,
                    });
                }
                if frontier.output() < applied.frontier().output() || frontier.output() > activation
                {
                    return Err(SyncError::PresentationMismatch {
                        operation: applied.stamp().operation(),
                        expected: Some(lane.map()),
                        given: frontier,
                    });
                }
                latch_plan_at(lane.plan(), activation)?
            }
            // An empty group owns only a logical timeline; it has no audio
            // member whose unplayed accepted grid could be mistaken for sound.
            _ if self.members.is_empty() => latch_at(&self.grid, activation)?,
            _ if matches!(self.grid.state(), BeatGridState::Unavailable(_)) => None,
            AlignmentSource::Audible { frontier, .. } => {
                return Err(self.unmatched_applied(frontier.warp_map()));
            }
            AlignmentSource::Prepared(_) | AlignmentSource::Cued(_) => {
                return Err(SyncError::CapabilityUnavailable {
                    capability: SyncCapability::Alignment,
                });
            }
        };
        let Some(local) = local else {
            return Ok(ModeEffect::Deferred {
                required: MapRegion::point(MapPosition::Session(activation)),
            });
        };
        self.local_effect(local, activation)
    }

    fn unmatched_applied(&self, given: Option<kithara_warp::WarpMapRevision>) -> SyncError {
        let [lane] = self.applied.as_slice() else {
            return SyncError::CapabilityUnavailable {
                capability: SyncCapability::Alignment,
            };
        };
        SyncError::AudibleMapMismatch {
            member_id: lane.member(),
            expected: Some(lane.map()),
            given,
        }
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
            descent: Box::new(ParentFact::Withdrawn(descent)),
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
        let (grid, descent) =
            self.derived_grid(axis.epoch(), local.anchor, local.meter, None, None)?;
        validate_successor(&self.grid, &grid, Withdrawal::Refused)?;
        Ok(ModeEffect::Changed {
            timeline: Timeline::Local(Some(local)),
            grid,
            descent: Box::new(descent),
            at,
            release: None,
        })
    }
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// The next grid revision following `anchor`, and the segment it hands
    /// every direct child group.
    pub(super) fn derived_grid(
        &self,
        epoch: SessionEpoch,
        anchor: SessionAnchor,
        meter: Option<MeterFacts>,
        output_transport: Option<TransportRevision>,
        execution_floor: Option<SessionFrame>,
    ) -> Result<(BeatGridSnapshot, ParentFact), SyncError> {
        let grid =
            BeatGridSnapshot::session(self.grid.id(), self.next_revision()?, epoch, anchor, meter);
        let mut segment = ParentGridUpdate::new(grid.stamp(), epoch, anchor, meter);
        if let Some(revision) = output_transport {
            segment = segment.with_output_transport(revision);
        }
        if let Some(floor) = execution_floor {
            segment = segment.with_execution_floor(floor);
        }
        Ok((grid, ParentFact::Segment(segment)))
    }

    pub(super) fn withdrawn_grid(&self) -> Result<BeatGridSnapshot, SyncError> {
        Ok(BeatGridSnapshot::unavailable(
            self.grid.id(),
            self.next_revision()?,
            self.grid.axis(),
        ))
    }

    pub(super) fn reserve_operation(&self) -> Result<SyncOperationId, SyncError> {
        self.next_operation
            .ok_or_else(|| SyncError::OperationIdExhausted {
                group_id: self.grid.id(),
            })
    }

    /// Stages and validates `effect` in full, then commits it.
    ///
    /// The operation itself takes the first identity; retargets and handoffs
    /// its staging mints take the ones after it.
    fn commit(
        &mut self,
        effect: ModeEffect,
        entry: Option<SyncEntry>,
    ) -> Result<SyncAdmission, SyncError> {
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
                let mut staged =
                    self.stage(grid, timeline, self.parent, Some(*descent), takeover)?;
                if let Some((_, prior)) = self.before_entry
                    && timeline == prior.timeline()
                {
                    for lane in &mut staged.applied {
                        lane.restore_local_lock(prior.grid(), staged.grid.stamp());
                    }
                }
                if !matches!(timeline, Timeline::Host)
                    && let Some((held, _)) = self.before_entry
                    && let Some(member) = self
                        .pending
                        .iter()
                        .find(|pending| pending.operation() == held)
                    && self.applied_of(member.member()).is_none()
                {
                    staged
                        .pending
                        .retain(|pending| pending.member() != member.member());
                }
                if let Some(transport) = release {
                    staged.pending = self.handoffs(&staged.grid, operation, transport, at)?;
                }
                if let Some(entry) = entry {
                    if let Some(issued) = self.stage_sync_entry(&mut staged, entry)? {
                        staged.before_entry = self
                            .before_entry
                            .map(|(_, prior)| (issued, prior))
                            .or_else(|| {
                                self.timeline
                                    .prior(self.grid.stamp())
                                    .map(|prior| (issued, prior))
                            });
                    }
                } else if !matches!(timeline, Timeline::Host) {
                    staged.before_entry = None;
                }
                (Committed::Changed(timeline), Some(staged))
            }
            ModeEffect::Unchanged => {
                if let Some(entry) = entry {
                    let takeover = Takeover {
                        commit: None,
                        next_operation,
                    };
                    let mut staged = self.stage(
                        self.grid.clone(),
                        self.timeline,
                        self.parent,
                        None,
                        takeover,
                    )?;
                    if let Some(issued) = self.stage_sync_entry(&mut staged, entry)? {
                        staged.before_entry = self.before_entry.map(|(_, prior)| (issued, prior));
                        (Committed::Changed(self.timeline), Some(staged))
                    } else {
                        (Committed::Unchanged, None)
                    }
                } else {
                    (Committed::Unchanged, None)
                }
            }
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
        f64::from(*tempo.value()) / SECONDS_PER_MINUTE,
        axis.sample_rate(),
    )?;
    Ok(Some(LocalTimeline { anchor, meter }))
}

/// Reads only the immutable target trajectory whose map the executor has
/// already presented. A newer accepted group grid may not have sounded yet.
fn latch_plan_at(plan: &WarpPlan, frame: SessionFrame) -> Result<Option<LocalTimeline>, SyncError> {
    let Some(MapAxis::Session(axis)) = plan.output_axis() else {
        return Ok(None);
    };
    let (BeatGridQuery::Resolved(beat), BeatGridQuery::Resolved(tempo)) =
        (plan.target_beat_at(frame), plan.target_tempo_at(frame))
    else {
        return Ok(None);
    };
    let meter = match plan.target_meter_at(*beat.value()) {
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
        f64::from(tempo) / SECONDS_PER_MINUTE,
        axis.sample_rate(),
    )?;
    Ok(Some(LocalTimeline { anchor, meter }))
}
