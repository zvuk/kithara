#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod host_seek;
#[cfg(test)]
mod tests;

use std::num::NonZeroU32;

use kithara_warp::{
    AssetFrame, BeatEstimate, BeatGrid, BeatGridId, BeatGridQuery, BeatGridRevision,
    BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatsPerMinute,
    DEFAULT_TEMPO_SMOOTHING_SECONDS, LoadGeneration, MapAxis, MapPoint, MapPosition, MapRegion,
    SessionAnchor, SessionAxis, SessionBeat, SessionEpoch, SessionFrame, SyncAdmission,
    SyncApplied, SyncCapability, SyncError, SyncGroup, SyncGroupSnapshot, SyncIntent, SyncMember,
    SyncMemberKind, SyncMode, SyncOperation, SyncOperationId, SyncRejected, SyncStatusSnapshot,
    TopologyRevision, TopologyStamp, TransportRevision, WarpMapRevision,
};

use super::{
    DeckGrid, TempoSource,
    prepare::{FreePreparing, PreparedDisposition, PreparedSync},
    topology::materialize_topology,
    transaction,
};

/// Minutes are how a tempo is spoken; beats per second is how it is counted.
const SECONDS_PER_MINUTE: f64 = 60.0;

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
    #[field(get, vis = "pub(crate)")]
    preparing: Option<FreePreparing>,
    locked: Option<SyncApplied>,
    topology_revision: TopologyRevision,
    members: Vec<SyncMember<G>>,
    /// Seconds this group's tempo approaches a new target over.
    #[field(with)]
    tempo_smoothing_seconds: f64,
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Makes a Free operation ackable only after the worker installs its map.
    pub(crate) fn adopt_free(&mut self, receipt: crate::worker::FreeAdoptionReceipt) -> bool {
        let Some(preparing) = self.preparing.clone() else {
            return false;
        };
        if preparing.operation != receipt.operation()
            || preparing.warp_map != receipt.warp_map()
            || (preparing.load, preparing.transport) != (receipt.load(), receipt.transport())
            || self.generations != (receipt.load(), receipt.transport())
        {
            return false;
        }
        self.preparing = None;
        let crate::worker::FreeAdoptionReceipt::Installed(receipt) = receipt else {
            self.prepared = None;
            return true;
        };
        self.prepared = Some(PreparedSync {
            operation: preparing.operation,
            warp_map: preparing.warp_map,
            source: receipt.source,
            activation: receipt.output,
            activation_beat: receipt.activation_beat,
            target: preparing.target,
            disposition: PreparedDisposition::Free,
        });
        true
    }
    /// Returns the load generation and transport revision of the last
    /// accepted transport operation.
    #[must_use]
    pub const fn generations(&self) -> (LoadGeneration, TransportRevision) {
        self.generations
    }

    /// Returns this group's current synchronization mode.
    #[must_use]
    pub(crate) const fn mode(&self) -> SyncMode {
        self.mode
    }

    /// Every directly nested group, for the owner to push committed state into.
    pub fn nested_groups_mut(&mut self) -> impl Iterator<Item = &mut G> {
        self.members.iter_mut().filter_map(|member| match member {
            SyncMember::Group { group, .. } => Some(group.as_mut()),
            SyncMember::Grid { .. } => None,
        })
    }

    /// Releases the parent alignment of one retained nested member.
    ///
    /// The member remains part of this topology; only its parent-owned
    /// alignment is invalid after the member completes a Free handoff.
    pub fn release_nested_alignment(&mut self, member_id: BeatGridId) -> Result<(), SyncError> {
        let Some(alignment) = self.members.iter_mut().find_map(|member| match member {
            SyncMember::Group {
                alignment, group, ..
            } if group.id() == member_id => Some(alignment),
            SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
        }) else {
            return Err(SyncError::MemberNotFound {
                group_id: self.grid.id(),
                member_id,
            });
        };
        *alignment = None;
        self.topology_revision =
            super::topology::next_topology_revision(self.grid.id(), self.topology_revision)?;
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
                }
                | SyncOperation::Sync {
                    intent: SyncIntent::AlignNow,
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
                let anchor = self.approached_anchor(now, beat, tempo).map_err(|_| {
                    SyncError::InvalidGroupGridState {
                        state: self.grid.state(),
                    }
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

    /// The group's beat line approaching `tempo` from the tempo it plays now.
    ///
    /// A member follows this line, so the approach belongs to the group and not
    /// to any member's own smoother: every member reads one curve and none of
    /// them can drift by a smoother history of its own.
    fn approached_anchor(
        &self,
        now: SessionFrame,
        beat: SessionBeat,
        tempo: BeatsPerMinute,
    ) -> Result<SessionAnchor, SyncError> {
        let axis = self.session_axis()?;
        let target = f64::from(tempo) / SECONDS_PER_MINUTE;
        let origin = MapPoint::new(self.grid.stamp(), MapPosition::Session(now));
        let playing = match (self.grid.state(), self.grid.tempo_at(origin)) {
            (_, BeatGridQuery::Resolved(estimate)) => {
                f64::from(*estimate.value()) / SECONDS_PER_MINUTE
            }
            (BeatGridState::Unavailable(_), _) => target,
            (state, _) => return Err(SyncError::InvalidGroupGridState { state }),
        };
        SessionAnchor::new(now, beat, playing, axis)
            .and_then(|anchor| anchor.retarget(now, target, self.tempo_smoothing_seconds))
            .map_err(|_| SyncError::InvalidGroupGridState {
                state: self.grid.state(),
            })
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

    /// Builds this deck's session grid from the owner's anchor.
    ///
    /// The anchor carries the session axis, so the deck adopts the owner's
    /// rate and epoch rather than reconstructing them from its own replica.
    /// An axis only changes across an epoch boundary, and that boundary is an
    /// unavailable grid, so a deck that still holds the previous axis takes
    /// that step first and adopts the anchor once the axes agree. An anchor
    /// that moves the axis without succeeding the epoch is a disagreement
    /// rather than a route change, and the deck keeps its committed grid.
    /// A deck whose grid is unavailable holds no live axis to move, so it
    /// adopts the anchor on the axis it already carries.
    fn session_candidate(&self, anchor: SessionAnchor) -> Result<BeatGridSnapshot, SyncError> {
        let axis = self.session_axis()?;
        if self.grid.state() == BeatGridState::Live && axis != anchor.axis() {
            if !is_successor_epoch(axis.epoch(), anchor.axis().epoch()) {
                return Err(SyncError::GridAxisChanged {
                    expected: MapAxis::Session(axis),
                    given: MapAxis::Session(anchor.axis()),
                });
            }
            return self.unavailable_axis_candidate(anchor.axis());
        }
        let revision = self.next_grid_revision()?;
        Ok(BeatGridSnapshot::session(
            self.grid.id(),
            revision,
            axis.epoch(),
            anchor,
            None,
        ))
    }

    /// Steps this deck onto `axis` with an unavailable grid.
    fn unavailable_axis_candidate(&self, axis: SessionAxis) -> Result<BeatGridSnapshot, SyncError> {
        let revision = self.next_grid_revision()?;
        Ok(BeatGridSnapshot::unavailable(
            self.grid.id(),
            revision,
            MapAxis::Session(axis),
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
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Records the parent's committed session anchor; under
    /// [`SyncMode::HostSync`] republishes this group's session grid on it.
    ///
    /// A preparation not yet presented, or a Free handoff not yet installed,
    /// was planned in output frames of the previous axis, so crossing an axis
    /// boundary drops it.
    pub fn publish_session_anchor(&mut self, anchor: SessionAnchor) -> Result<(), SyncError> {
        if self.mode == SyncMode::HostSync {
            let crosses_axis = self.crosses_axis_boundary(anchor);
            let candidate = self.session_candidate(anchor)?;
            self.publish_grid(candidate)?;
            if crosses_axis {
                self.prepared = None;
                self.preparing = None;
            }
        }
        self.parent_anchor = Some(anchor);
        Ok(())
    }

    /// The locked activation the same-axis `anchor` has not reached, moved
    /// onto the anchor's frame for its beat under the next warp map revision.
    ///
    /// A tempo commit must not move the music off its beat, but the deck's
    /// pending seek decides whether the activation can still move, so nothing
    /// changes until the successor is adopted.
    pub(crate) fn reanchored_prepared(
        &self,
        anchor: SessionAnchor,
    ) -> Result<Option<PreparedSync>, SyncError> {
        let Some(prepared) = self.prepared.filter(|prepared| {
            prepared.disposition == PreparedDisposition::Lock
                && prepared.activation_beat >= anchor.beat()
                && !self.crosses_axis_boundary(anchor)
        }) else {
            return Ok(None);
        };
        let activation = anchor.frame_at(prepared.activation_beat).map_err(|_| {
            SyncError::InvalidGroupGridState {
                state: self.grid.state(),
            }
        })?;
        if activation == prepared.activation {
            return Ok(None);
        }
        let warp_map =
            self.warp_map
                .checked_next()
                .ok_or_else(|| SyncError::WarpMapRevisionExhausted {
                    group_id: self.grid.id(),
                })?;
        Ok(Some(PreparedSync {
            warp_map,
            activation,
            ..prepared
        }))
    }

    /// Whether committing the same-axis `anchor` moves a settled deck's
    /// target tempo, so its audible mapping must be replaced.
    pub(crate) fn retargets_tempo(&self, anchor: SessionAnchor) -> bool {
        self.mode == SyncMode::HostSync
            && self.grid.state() == BeatGridState::Live
            && self.prepared.is_none()
            && self.preparing.is_none()
            && !self.crosses_axis_boundary(anchor)
            && self.parent_anchor.is_some_and(|previous| {
                previous.target_beats_per_second() != anchor.target_beats_per_second()
            })
    }

    /// Adopts a successor produced by [`Self::reanchored_prepared`].
    pub(crate) fn adopt_reanchored(&mut self, successor: PreparedSync) {
        self.warp_map = successor.warp_map;
        self.prepared = Some(successor);
    }

    /// Whether committing `anchor` steps this deck's live grid onto the
    /// successor axis, withdrawing whatever was prepared on the current one.
    pub(crate) fn crosses_axis_boundary(&self, anchor: SessionAnchor) -> bool {
        self.mode == SyncMode::HostSync
            && self.grid.state() == BeatGridState::Live
            && self.session_axis().is_ok_and(|axis| {
                axis != anchor.axis() && is_successor_epoch(axis.epoch(), anchor.axis().epoch())
            })
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
            preparing: None,
            locked: None,
            topology_revision: TopologyRevision::first(),
            unavailable: None,
            waiting: None,
            tempo_smoothing_seconds: DEFAULT_TEMPO_SMOOTHING_SECONDS,
        }
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
        if prepared.disposition == PreparedDisposition::Free {
            self.mode = SyncMode::Off;
            self.tempo = TempoSource::Inherited;
            self.grid = self.unavailable_session_candidate()?;
            self.locked = None;
        } else {
            self.locked = Some(given);
        }
        self.prepared = None;
        Ok(if prepared.disposition == PreparedDisposition::Free {
            self.status()
        } else {
            SyncStatusSnapshot::Locked {
                applied: given,
                phase_error_frames: 0.0,
            }
        })
    }

    fn status(&self) -> SyncStatusSnapshot {
        transaction::status(
            TopologyStamp::new(self.grid.id(), self.topology_revision),
            &transaction::StatusSlots {
                unavailable: self.unavailable,
                waiting: self.waiting,
                preparing: self.preparing.clone(),
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
                preparing: &mut self.preparing,
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
