#[cfg(test)]
mod tests;

use std::num::NonZeroU32;

use kithara_warp::{
    AssetFrame, BeatEstimate, BeatGrid, BeatGridId, BeatGridQuery, BeatGridRevision,
    BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatsPerMinute, LoadGeneration, MapAxis,
    MapPoint, MapPosition, MapRegion, SessionAnchor, SessionAxis, SessionBeat, SessionEpoch,
    SessionFrame, SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncIntent, SyncMember, SyncMemberKind, SyncMode, SyncOperation,
    SyncOperationId, SyncRejected, SyncStatusSnapshot, TopologyRevision, TopologyStamp,
    TransportRevision, WarpMapRevision,
};

use super::{
    DeckGrid, TempoSource, prepare::PreparedSync, topology::materialize_topology, transaction,
};

/// Canonical mutable state for one recursive synchronization group.
///
/// `G` is the concrete nested-group representation. The group owns every live
/// member exclusively; callers interact through transactions or closure-based
/// access so member references cannot escape the owning lock.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct GroupState<G: SyncGroup<NestedGroup = G>> {
    grid: BeatGridSnapshot,
    next_operation: Option<SyncOperationId>,
    unavailable: Option<(SyncOperationId, SyncCapability)>,
    waiting: Option<(SyncOperationId, MapRegion)>,
    member_kind: SyncMemberKind,
    mode: SyncMode,
    tempo: TempoSource,
    generations: (LoadGeneration, TransportRevision),
    parent_anchor: Option<SessionAnchor>,
    warp_map: WarpMapRevision,
    #[field(get, vis = "pub(crate)", copy)]
    prepared: Option<PreparedSync>,
    locked: Option<SyncApplied>,
    topology_revision: TopologyRevision,
    members: Vec<SyncMember<G>>,
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Creates an empty group around an already-published grid.
    #[must_use]
    pub fn new(grid: BeatGridSnapshot, member_kind: SyncMemberKind, mode: SyncMode) -> Self {
        Self {
            grid,
            mode,
            member_kind,
            members: Vec::new(),
            next_operation: Some(SyncOperationId::first()),
            tempo: TempoSource::Inherited,
            generations: (LoadGeneration::first(), TransportRevision::first()),
            parent_anchor: None,
            warp_map: WarpMapRevision::first(),
            prepared: None,
            locked: None,
            topology_revision: TopologyRevision::first(),
            unavailable: None,
            waiting: None,
        }
    }

    /// Returns the load generation and transport revision of the last
    /// accepted transport operation.
    #[must_use]
    pub const fn generations(&self) -> (LoadGeneration, TransportRevision) {
        self.generations
    }

    /// Every directly nested group, for the owner to push committed state into.
    pub fn nested_groups_mut(&mut self) -> impl Iterator<Item = &mut G> {
        self.members.iter_mut().filter_map(|member| match member {
            SyncMember::Group { group, .. } => Some(group.as_mut()),
            SyncMember::Grid { .. } => None,
        })
    }

    /// Records the parent's committed session anchor; under
    /// [`SyncMode::HostSync`] republishes this group's session grid on it.
    pub fn publish_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError> {
        if self.mode == SyncMode::HostSync {
            let candidate = self.session_candidate(anchor)?;
            self.publish_grid(candidate)?;
        }
        self.parent_anchor = Some(anchor);
        Ok(())
    }

    /// Commits a deck state change and its session grid as one transaction.
    pub(crate) fn transact_at(
        &mut self,
        operation: SyncOperation<G>,
        now: SessionFrame,
    ) -> Result<(SyncAdmission, Option<DeckGrid>), SyncRejected<G>> {
        let state = if operation.target() == self.grid.id() {
            match &operation {
                SyncOperation::Sync {
                    intent: SyncIntent::Enable,
                    ..
                } => Some((SyncMode::HostSync, TempoSource::Inherited)),
                SyncOperation::Sync {
                    intent: SyncIntent::Disable,
                    ..
                } => self
                    .seed_local_tempo()
                    .map(|tempo| (SyncMode::LocalSync, TempoSource::Local(tempo))),
                SyncOperation::Sync {
                    intent: SyncIntent::Free,
                    ..
                } => Some((SyncMode::Off, TempoSource::Inherited)),
                SyncOperation::Tempo { tempo, .. } if self.mode == SyncMode::LocalSync => {
                    Some((SyncMode::LocalSync, TempoSource::Local(*tempo)))
                }
                _ => None,
            }
        } else {
            None
        };
        let candidate = state
            .map(|(mode, tempo)| self.state_grid(mode, tempo, now))
            .transpose();
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => return Err(SyncRejected::new(error, operation)),
        };
        if let Some(candidate) = &candidate
            && let Err(error) = self.validate_grid(&candidate.0)
        {
            return Err(SyncRejected::new(error, operation));
        }
        let admission = self.transact(operation)?;
        let projection = if matches!(admission, SyncAdmission::StateChanged { .. }) {
            candidate.map(|(grid, projection)| {
                self.grid = grid;
                projection
            })
        } else {
            None
        };
        Ok((admission, projection))
    }

    fn state_grid(
        &self,
        mode: SyncMode,
        tempo: TempoSource,
        now: SessionFrame,
    ) -> Result<(BeatGridSnapshot, DeckGrid), SyncError> {
        match (mode, tempo) {
            (SyncMode::HostSync, _) => self
                .parent_anchor
                .map_or_else(
                    || self.unavailable_session_candidate(),
                    |anchor| self.session_candidate(anchor),
                )
                .map(|grid| (grid, DeckGrid::Host)),
            (SyncMode::LocalSync, TempoSource::Local(tempo)) => {
                let origin = MapPoint::new(self.grid.stamp(), MapPosition::Session(now));
                let beat = match (self.grid.state(), self.grid.beat_at(origin)) {
                    (_, BeatGridQuery::Resolved(estimate)) => f64::from(*estimate.value().value()),
                    (BeatGridState::Unavailable(_), _) => 0.0,
                    (state, _) => return Err(SyncError::InvalidGroupGridState { state }),
                };
                let beat =
                    SessionBeat::new(beat).map_err(|_| SyncError::InvalidGroupGridState {
                        state: self.grid.state(),
                    })?;
                let anchor = SessionAnchor::new(
                    now,
                    beat,
                    f64::from(tempo) / 60.0,
                    self.grid.axis().sample_rate(),
                )
                .map_err(|_| SyncError::InvalidGroupGridState {
                    state: self.grid.state(),
                })?;
                self.session_candidate(anchor)
                    .map(|grid| (grid, DeckGrid::Local(anchor)))
            }
            (SyncMode::LocalSync, TempoSource::Inherited) => {
                Err(SyncError::InvalidGroupGridState {
                    state: self.grid.state(),
                })
            }
            (SyncMode::Off, _) => self
                .unavailable_session_candidate()
                .map(|grid| (grid, DeckGrid::Off)),
        }
    }

    fn session_axis(&self) -> Result<SessionAxis, SyncError> {
        match self.grid.axis() {
            MapAxis::Session(axis) => Ok(axis),
            axis => Err(SyncError::GridAxisChanged {
                expected: MapAxis::Session(SessionAxis::new(
                    axis.sample_rate(),
                    SessionEpoch::new(0),
                )),
                given: axis,
            }),
        }
    }

    fn next_grid_revision(&self) -> Result<BeatGridRevision, SyncError> {
        self.grid
            .revision()
            .checked_next()
            .ok_or_else(|| SyncError::TopologyRevisionExhausted {
                group_id: self.grid.id(),
            })
    }

    fn session_candidate(&self, anchor: SessionAnchor) -> Result<BeatGridSnapshot, SyncError> {
        let axis = self.session_axis()?;
        let revision = self.next_grid_revision()?;
        Ok(BeatGridSnapshot::session(
            self.grid.id(),
            revision,
            axis.epoch(),
            anchor,
            None,
        ))
    }

    fn unavailable_session_candidate(&self) -> Result<BeatGridSnapshot, SyncError> {
        let axis = self.session_axis()?;
        let revision = self.next_grid_revision()?;
        let epoch = if self.grid.state() == BeatGridState::Live {
            u64::from(axis.epoch())
                .checked_add(1)
                .map(SessionEpoch::new)
                .ok_or_else(|| SyncError::TopologyRevisionExhausted {
                    group_id: self.grid.id(),
                })?
        } else {
            axis.epoch()
        };
        Ok(BeatGridSnapshot::unavailable(
            self.grid.id(),
            revision,
            MapAxis::Session(SessionAxis::new(axis.sample_rate(), epoch)),
        ))
    }

    /// Publishes a later immutable grid snapshot for this stable owner.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when the candidate changes identity or axis, moves
    /// the revision backwards, or violates the group-grid lifecycle.
    pub fn publish_grid(&mut self, candidate: BeatGridSnapshot) -> Result<(), SyncError> {
        self.validate_grid(&candidate)?;
        if candidate.stamp() != self.grid.stamp() {
            self.grid = candidate;
        }
        Ok(())
    }

    fn validate_grid(&self, candidate: &BeatGridSnapshot) -> Result<(), SyncError> {
        let given = candidate.stamp();
        if given.grid_id() != self.grid.id() {
            return Err(SyncError::GridIdentityMismatch {
                expected: self.grid.id(),
                given: given.grid_id(),
            });
        }
        let candidate_state = candidate.state();
        let candidate_axis = candidate.axis();
        let current_state = self.grid.state();
        let expected_axis = self.grid.axis();
        if given == self.grid.stamp() {
            return Ok(());
        }
        if given.revision() <= self.grid.revision() {
            return Err(SyncError::StaleGridRevision {
                given,
                current: self.grid.stamp(),
            });
        }
        if !matches!(
            candidate_state,
            BeatGridState::Live | BeatGridState::Unavailable(_)
        ) {
            return Err(SyncError::InvalidGroupGridState {
                state: candidate_state,
            });
        }
        let axis_is_valid = match (expected_axis, candidate_axis) {
            (MapAxis::Session(current), MapAxis::Session(next))
                if is_successor_epoch(current.epoch(), next.epoch())
                    && matches!(candidate_state, BeatGridState::Unavailable(_)) =>
            {
                true
            }
            (MapAxis::Session(current), MapAxis::Session(next))
                if next.epoch() == current.epoch() =>
            {
                match (current_state, candidate_state) {
                    (BeatGridState::Live, BeatGridState::Live)
                    | (BeatGridState::Unavailable(_), BeatGridState::Unavailable(_)) => {
                        current.sample_rate() == next.sample_rate()
                    }
                    (BeatGridState::Unavailable(_), BeatGridState::Live) => true,
                    (BeatGridState::Live, BeatGridState::Unavailable(_)) => {
                        return Err(SyncError::InvalidGroupGridTransition {
                            from: current_state,
                            to: candidate_state,
                        });
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if !axis_is_valid {
            return Err(SyncError::GridAxisChanged {
                expected: expected_axis,
                given: candidate_axis,
            });
        }
        Ok(())
    }

    /// Publishes a later unavailable session-axis snapshot.
    ///
    /// # Errors
    ///
    /// Forwards validation failures from [`Self::publish_grid`].
    pub fn publish_unavailable_grid(
        &mut self,
        stamp: BeatGridStamp,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
    ) -> Result<(), SyncError> {
        self.publish_grid(BeatGridSnapshot::unavailable(
            stamp.grid_id(),
            stamp.revision(),
            MapAxis::Session(SessionAxis::new(sample_rate, epoch)),
        ))
    }

    /// The tempo a `Disable` latches from the group or its first live grid.
    pub(crate) fn seed_local_tempo(&self) -> Option<BeatsPerMinute> {
        self.deck_tempo().or_else(|| {
            self.members
                .iter()
                .find_map(|member| match member {
                    SyncMember::Grid { grid, .. } => {
                        let snapshot = grid.snapshot();
                        let origin = MapPoint::new(
                            snapshot.stamp(),
                            MapPosition::Asset(AssetFrame::new(0.0).ok()?),
                        );
                        Some(snapshot.tempo_at(origin))
                    }
                    SyncMember::Group { .. } => None,
                })
                .and_then(resolved_tempo)
        })
    }

    /// The tempo this deck plays at: its local tempo, else the tempo of its
    /// live session grid at the session origin.
    #[must_use]
    pub fn deck_tempo(&self) -> Option<BeatsPerMinute> {
        if let TempoSource::Local(tempo) = self.tempo {
            return Some(tempo);
        }
        (self.grid.state() == BeatGridState::Live)
            .then(|| {
                let origin = MapPoint::new(
                    self.grid.stamp(),
                    MapPosition::Session(SessionFrame::new(0)),
                );
                self.grid.tempo_at(origin)
            })
            .and_then(resolved_tempo)
    }

    /// Creates an empty group whose session-axis grid is not available yet.
    #[must_use]
    pub fn unavailable(
        id: BeatGridId,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
        member_kind: SyncMemberKind,
        mode: SyncMode,
    ) -> Self {
        Self::new(
            BeatGridSnapshot::unavailable(
                id,
                BeatGridRevision::first(),
                MapAxis::Session(SessionAxis::new(sample_rate, epoch)),
            ),
            member_kind,
            mode,
        )
    }

    /// Executes `dispatch` against one direct nested group without exposing a
    /// reference outside the call.
    pub fn with_group<R, F>(&self, id: BeatGridId, dispatch: F) -> Option<R>
    where
        R: 'static,
        F: FnOnce(&G) -> R,
    {
        let group = self.members.iter().find_map(|member| match member {
            SyncMember::Group { group, .. } if group.id() == id => Some(group.as_ref()),
            SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
        })?;
        Some(dispatch(group))
    }
}

impl<G: SyncGroup<NestedGroup = G>> BeatGrid for GroupState<G> {
    delegate::delegate! {
        to self.grid {
            fn id(&self) -> BeatGridId;
            #[call(clone)]
            fn snapshot(&self) -> BeatGridSnapshot;
        }
    }
}

impl<G: SyncGroup<NestedGroup = G>> SyncGroup for GroupState<G> {
    type NestedGroup = G;

    fn acknowledge(&mut self, given: SyncApplied) -> Result<SyncStatusSnapshot, SyncError> {
        if let Some(locked) = self.locked
            && locked.operation() == given.operation()
        {
            return Err(SyncError::DuplicateAcknowledgement {
                operation: given.operation(),
            });
        }
        let prepared = self.prepared.ok_or(SyncError::NoPreparedOperation)?;
        if given.operation() != prepared.operation {
            return Err(SyncError::StaleAcknowledgement {
                expected: prepared.operation,
                given: given.operation(),
            });
        }
        let (load, transport) = self.generations;
        let expected = SyncApplied::builder()
            .group(self.grid.stamp())
            .load(load)
            .frontier(given.frontier())
            .operation(prepared.operation)
            .topology(TopologyStamp::new(self.grid.id(), self.topology_revision))
            .transport(transport)
            .warp_map(prepared.warp_map)
            .build();
        if given != expected {
            return Err(SyncError::AppliedMismatch {
                expected: Box::new(expected),
                given: Box::new(given),
            });
        }
        self.locked = Some(given);
        self.prepared = None;
        Ok(SyncStatusSnapshot::Locked {
            applied: given,
            phase_error_frames: 0.0,
        })
    }

    fn status(&self) -> SyncStatusSnapshot {
        transaction::status(
            TopologyStamp::new(self.grid.id(), self.topology_revision),
            &transaction::StatusSlots {
                unavailable: self.unavailable,
                waiting: self.waiting,
                prepared: self.prepared,
                locked: self.locked,
            },
        )
    }

    fn topology(&self) -> Result<SyncGroupSnapshot, SyncError> {
        materialize_topology(&self.grid, self.topology_revision, &self.members)
    }

    fn transact(&mut self, operation: SyncOperation<G>) -> Result<SyncAdmission, SyncRejected<G>> {
        let seed = self.seed_local_tempo();
        transaction::transact(
            &self.grid,
            transaction::GroupSlots {
                next_operation: &mut self.next_operation,
                unavailable: &mut self.unavailable,
                waiting: &mut self.waiting,
                mode: &mut self.mode,
                tempo: &mut self.tempo,
                generations: &mut self.generations,
                warp_map: &mut self.warp_map,
                prepared: &mut self.prepared,
                locked: &mut self.locked,
                topology_revision: &mut self.topology_revision,
                members: &mut self.members,
            },
            self.member_kind,
            seed,
            operation,
        )
    }
}

fn is_successor_epoch(current: SessionEpoch, next: SessionEpoch) -> bool {
    u64::from(current)
        .checked_add(1)
        .is_some_and(|successor| successor == u64::from(next))
}

fn resolved_tempo(query: BeatGridQuery<BeatEstimate<BeatsPerMinute>>) -> Option<BeatsPerMinute> {
    match query {
        BeatGridQuery::Resolved(estimate) => Some(*estimate.value()),
        _ => None,
    }
}
