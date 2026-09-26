use std::num::NonZeroU32;

use kithara_signal::SessionEpoch;
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridRevision, BeatGridSnapshot, BeatGridStamp, BeatGridState,
    BeatsPerMinute, MapAxis, SessionAxis, WarpMapRevision,
};

use super::{
    descent::{Parent, Takeover},
    lifecycle::Applied,
    mutation::{materialize_topology, routed_group},
    preparation::Pending,
    timeline::{Blocked, PriorTimeline, Timeline},
};
use crate::{
    ParentFact, ParentGridUpdate, SessionAxisUpdate, SyncAdmission, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncOperationId,
    SyncReceipt, SyncRejected, SyncStaged, SyncStatusSnapshot, SyncTransition, TopologyRevision,
    TopologyStamp,
};

/// Canonical mutable state for one recursive synchronization group.
///
/// `G` is the concrete nested-group representation. The group owns every live
/// member exclusively; callers interact through transactions or closure-based
/// access so member references cannot escape the owning lock.
pub struct GroupState<G: SyncGroup<NestedGroup = G>> {
    pub(super) grid: BeatGridSnapshot,
    pub(super) timeline: Timeline,
    pub(super) before_entry: Option<(SyncOperationId, PriorTimeline)>,
    pub(super) parent: Option<Parent>,
    pub(super) next_operation: Option<SyncOperationId>,
    pub(super) blocked: Option<Blocked>,
    pub(super) pending: Vec<Pending>,
    pub(super) applied: Vec<Applied>,
    pub(super) next_map: Option<WarpMapRevision>,
    pub(super) member_kind: SyncMemberKind,
    pub(super) topology_revision: TopologyRevision,
    pub(super) members: Vec<SyncMember<G>>,
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Creates an empty group in [`SyncMode::Off`] around an externally
    /// published grid.
    #[must_use]
    pub fn new(grid: BeatGridSnapshot, member_kind: SyncMemberKind) -> Self {
        Self {
            grid,
            member_kind,
            timeline: Timeline::Off,
            before_entry: None,
            parent: None,
            members: Vec::new(),
            next_operation: Some(SyncOperationId::first()),
            topology_revision: TopologyRevision::first(),
            blocked: None,
            pending: Vec::new(),
            applied: Vec::new(),
            next_map: Some(WarpMapRevision::first()),
        }
    }

    /// Returns the relation between this group's timeline and its parent.
    #[must_use]
    pub const fn mode(&self) -> SyncMode {
        self.timeline.mode()
    }

    /// Returns the tempo this group's own or inherited timeline approaches.
    ///
    /// A group in [`SyncMode::Off`] claims no tempo, and neither does a group
    /// whose timeline has no geometry yet.
    #[must_use]
    pub fn tempo(&self) -> Option<BeatsPerMinute> {
        self.timeline
            .tempo(self.parent.and_then(Parent::segment).as_ref())
    }

    /// Publishes the session trajectory of a group in [`SyncMode::Off`] from
    /// its external timeline owner, and passes it on to every direct child
    /// group as its parent segment.
    ///
    /// Returns every preparation the publication issued and withdrew across
    /// the subtree.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when the group derives its own grid, when the
    /// segment does not succeed the current grid, or when any child group
    /// refuses it; nothing changes then.
    pub fn publish_session(
        &mut self,
        update: ParentGridUpdate,
    ) -> Result<SyncTransition, SyncError> {
        let stamp = update.parent();
        let candidate = BeatGridSnapshot::session(
            stamp.grid_id(),
            stamp.revision(),
            update.epoch(),
            update.anchor(),
            update.meter(),
        );
        self.publish(candidate, Some(ParentFact::Segment(update)))
    }

    /// Publishes a later unavailable session-axis snapshot, moving every
    /// direct child group onto the axis when it changes.
    ///
    /// Returns every preparation the publication issued and withdrew across
    /// the subtree.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] when the group derives its own grid, when the
    /// snapshot does not succeed the current grid, or when any child group
    /// refuses the axis; nothing changes then.
    pub fn publish_unavailable_grid(
        &mut self,
        stamp: BeatGridStamp,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
    ) -> Result<SyncTransition, SyncError> {
        self.publish_grid(BeatGridSnapshot::unavailable(
            stamp.grid_id(),
            stamp.revision(),
            MapAxis::Session(SessionAxis::new(sample_rate, epoch)),
        ))
    }

    /// Publishes a later immutable grid snapshot from the external timeline
    /// owner of a group in [`SyncMode::Off`].
    pub(super) fn publish_grid(
        &mut self,
        candidate: BeatGridSnapshot,
    ) -> Result<SyncTransition, SyncError> {
        let descent = match candidate.axis() {
            MapAxis::Session(axis) if candidate.axis() != self.grid.axis() => {
                Some(ParentFact::Axis(SessionAxisUpdate::new(axis)))
            }
            _ => None,
        };
        self.publish(candidate, descent)
    }

    fn publish(
        &mut self,
        candidate: BeatGridSnapshot,
        descent: Option<ParentFact>,
    ) -> Result<SyncTransition, SyncError> {
        if !matches!(self.timeline, Timeline::Off) {
            return Err(SyncError::GridOwnedByMode { mode: self.mode() });
        }
        if candidate.stamp() == self.grid.stamp() {
            return Ok(SyncTransition::default());
        }
        validate_successor(&self.grid, &candidate, Withdrawal::Refused)?;
        let takeover = Takeover {
            commit: None,
            next_operation: self.next_operation,
        };
        let staged = self.stage(candidate, self.timeline, self.parent, descent, takeover)?;
        Ok(self.apply(Some(staged)))
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
        let mut group = Self::new(
            BeatGridSnapshot::unavailable(
                id,
                BeatGridRevision::first(),
                MapAxis::Session(SessionAxis::new(sample_rate, epoch)),
            ),
            member_kind,
        );
        group.timeline = Timeline::without_geometry(mode);
        group
    }

    /// Creates a group that owns `member` from birth, with a session-axis
    /// grid that is not available yet.
    ///
    /// The member is the group's own rather than one a caller attached, so
    /// there is no topology transaction to reject: it is admitted under the
    /// kind it already is, and it carries no alignment to restamp. A group
    /// whose one member is its own stable grid never changes topology, which
    /// is what lets that grid state a later revision on every load.
    #[must_use]
    pub fn owning(
        id: BeatGridId,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
        member: SyncMember<G>,
    ) -> Self {
        let member_kind = member.kind();
        let mut group = Self::unavailable(id, sample_rate, epoch, member_kind, SyncMode::Off);
        group.members.push(member);
        group
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

    pub(super) fn next_revision(&self) -> Result<BeatGridRevision, SyncError> {
        self.grid
            .revision()
            .checked_next()
            .ok_or_else(|| SyncError::GridRevisionExhausted {
                group_id: self.grid.id(),
            })
    }

    pub(super) fn topology_stamp(&self) -> TopologyStamp {
        TopologyStamp::new(self.grid.id(), self.topology_revision)
    }

    /// Keeps an unpresented leaf entry's prior timeline only while one
    /// pending decision on the same physical axis and load still owns it.
    /// A same-load replacement inherits custody; an axis or owner change
    /// severs it before a late rejection can restore an obsolete timeline.
    pub(super) fn reconcile_before_entry(
        &self,
        grid: &BeatGridSnapshot,
        timeline: Timeline,
        pending: &[Pending],
        custody: Option<(SyncOperationId, PriorTimeline)>,
    ) -> Option<(SyncOperationId, PriorTimeline)> {
        let (operation, prior) = custody?;
        if grid.axis() != self.grid.axis() || !matches!(timeline, Timeline::Host) {
            return None;
        }
        if pending
            .iter()
            .any(|held| held.operation() == operation && held.enters_map())
        {
            return Some((operation, prior));
        }
        let previous = self
            .pending
            .iter()
            .find(|held| held.operation() == operation)?;
        pending
            .iter()
            .find(|held| {
                held.member() == previous.member()
                    && held.load() == previous.load()
                    && held
                        .preparation()
                        .zip(previous.preparation())
                        .is_some_and(|(next, old)| {
                            next.stamp().transport() == old.stamp().transport()
                        })
                    && held.enters_map()
            })
            .map(|held| (held.operation(), prior))
    }

    /// The state of the latest map a member sounds through: locked while it
    /// follows the current group grid, converging while the grid moved on.
    fn applied_status(&self, topology: TopologyStamp) -> SyncStatusSnapshot {
        let Some(lane) = self
            .applied
            .iter()
            .max_by_key(|lane| lane.applied().stamp().operation())
        else {
            return SyncStatusSnapshot::Off { topology };
        };
        let (applied, phase_error_frames) = (lane.applied(), lane.phase_error_frames());
        if lane.locked_grid() == self.grid.stamp() {
            SyncStatusSnapshot::Locked {
                applied,
                phase_error_frames,
            }
        } else {
            SyncStatusSnapshot::Converging {
                applied,
                phase_error_frames,
            }
        }
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

    delegate::delegate! {
        to self {
            #[call(stage_descent)]
            fn stage_fact(&self, fact: ParentFact) -> Result<SyncStaged, SyncError>;
            #[call(apply_descent)]
            fn apply_staged(&mut self, staged: SyncStaged) -> SyncTransition;
            #[call(route)]
            fn transact(&mut self, operation: SyncOperation<G>) -> Result<SyncAdmission, SyncRejected<G>>;
            fn mode(&self) -> SyncMode;
        }
    }

    fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError> {
        let group_id = receipt.stamp().group().grid_id();
        if group_id == self.grid.id() {
            return self.record(receipt);
        }
        routed_group(&mut self.members, group_id)?
            .ok_or(SyncError::GroupNotFound { group_id })?
            .acknowledge(receipt)
    }

    fn status(&self) -> SyncStatusSnapshot {
        let topology = self.topology_stamp();
        if let Some(Blocked {
            operation,
            required,
        }) = self.blocked
        {
            return SyncStatusSnapshot::WaitingForGrid {
                operation,
                topology,
                required,
            };
        }
        let waiting = self
            .pending
            .iter()
            .filter(|pending| matches!(pending, Pending::Waiting { .. }))
            .max_by_key(|pending| pending.operation());
        let latest = waiting.or_else(|| {
            self.pending
                .iter()
                .max_by_key(|pending| pending.operation())
        });
        match latest {
            None => self.applied_status(topology),
            Some(Pending::Waiting {
                operation,
                required,
                ..
            }) => SyncStatusSnapshot::WaitingForGrid {
                operation: *operation,
                topology,
                required: *required,
            },
            Some(Pending::Prepared { preparation, .. }) => {
                let (warp_map, activation) = preparation.activation();
                SyncStatusSnapshot::Prepared {
                    operation: preparation.stamp().operation(),
                    topology: preparation.stamp().topology(),
                    warp_map,
                    activation,
                }
            }
        }
    }

    fn topology(&self) -> Result<SyncGroupSnapshot, SyncError> {
        materialize_topology(&self.grid, self.topology_revision, &self.members)
    }
}

/// Whether a same-epoch successor may drop a live grid's geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Withdrawal {
    /// A mode transition leaves the beat timeline it followed.
    Allowed,
    /// A publication on the same axis; only a new epoch drops geometry.
    Refused,
}

/// Validates `candidate` as the next revision of the group grid `current`.
pub(super) fn validate_successor(
    current: &BeatGridSnapshot,
    candidate: &BeatGridSnapshot,
    withdrawal: Withdrawal,
) -> Result<(), SyncError> {
    let given = candidate.stamp();
    if given.grid_id() != current.id() {
        return Err(SyncError::GridIdentityMismatch {
            expected: current.id(),
            given: given.grid_id(),
        });
    }
    let candidate_state = candidate.state();
    let candidate_axis = candidate.axis();
    let current_state = current.state();
    let expected_axis = current.axis();
    if given.revision() <= current.revision() {
        return Err(SyncError::StaleGridRevision {
            given,
            current: current.stamp(),
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
        (MapAxis::Session(current), MapAxis::Session(next)) if next.epoch() == current.epoch() => {
            match (current_state, candidate_state) {
                (BeatGridState::Live, BeatGridState::Live)
                | (BeatGridState::Unavailable(_), BeatGridState::Unavailable(_)) => {
                    current.sample_rate() == next.sample_rate()
                }
                (BeatGridState::Unavailable(_), BeatGridState::Live) => true,
                (BeatGridState::Live, BeatGridState::Unavailable(_))
                    if withdrawal == Withdrawal::Allowed =>
                {
                    current.sample_rate() == next.sample_rate()
                }
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

fn is_successor_epoch(current: SessionEpoch, next: SessionEpoch) -> bool {
    u64::from(current)
        .checked_add(1)
        .is_some_and(|successor| successor == u64::from(next))
}
