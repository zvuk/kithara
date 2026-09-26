use kithara_signal::SessionFrame;
use kithara_warp::{BeatGridSnapshot, BeatGridStamp, MapAxis, WarpMapRevision};

use super::{
    lifecycle::Applied,
    preparation::{Pending, Refreshed, transition},
    state::{GroupState, Withdrawal, validate_successor},
    timeline::{PriorTimeline, Timeline},
    transaction::take_operation,
};
use crate::{
    ParentFact, ParentGridUpdate, ParentWithdrawal, SessionAxisUpdate, SyncError, SyncGroup,
    SyncMember, SyncOperationId, SyncTransition,
};

/// The parent timeline a group last accepted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Parent {
    /// The parent's tempo and phase segment.
    Segment(ParentGridUpdate),
    /// The parent grid revision that has no geometry.
    Withdrawn(BeatGridStamp),
}

impl From<Parent> for BeatGridStamp {
    fn from(parent: Parent) -> Self {
        match parent {
            Parent::Segment(update) => update.parent(),
            Parent::Withdrawn(stamp) => stamp,
        }
    }
}

impl Parent {
    pub(super) const fn segment(self) -> Option<ParentGridUpdate> {
        match self {
            Self::Segment(update) => Some(update),
            Self::Withdrawn(_) => None,
        }
    }
}

/// One group's successor grid together with everything it moves: the
/// decisions and applied maps of its direct members, the identities they
/// spend, and the staged change of every direct child group, computed before
/// mutation.
#[derive(Debug)]
pub(super) struct Staged {
    pub(super) grid: BeatGridSnapshot,
    timeline: Timeline,
    pub(super) before_entry: Option<(SyncOperationId, PriorTimeline)>,
    parent: Option<Parent>,
    pub(super) pending: Vec<Pending>,
    pub(super) applied: Vec<Applied>,
    pub(super) next_map: Option<WarpMapRevision>,
    pub(super) next_operation: Option<SyncOperationId>,
    children: Vec<SyncStaged>,
}

/// A parent fact's complete effect on one subtree, computed against that
/// subtree's current state and not applied yet.
///
/// Only the group that staged it may apply it, and only while nothing else
/// changed the subtree in between.
#[derive(Debug)]
#[must_use]
pub struct SyncStaged(Option<Box<Staged>>);

/// Where a staged grid takes over, and the identities a retarget may spend.
#[derive(Clone, Copy)]
pub(super) struct Takeover {
    pub(super) commit: Option<SessionFrame>,
    pub(super) next_operation: Option<SyncOperationId>,
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    pub(super) fn stage_descent(&self, fact: ParentFact) -> Result<SyncStaged, SyncError> {
        let staged = match fact {
            ParentFact::Segment(update) => self.stage_segment(update)?,
            ParentFact::Withdrawn(withdrawal) => self.stage_withdrawal(withdrawal)?,
            ParentFact::Axis(update) => self.stage_axis(update)?,
            ParentFact::Joined(update) => self.stage_join(update)?,
        };
        Ok(SyncStaged(staged.map(Box::new)))
    }

    pub(super) fn apply_descent(&mut self, staged: SyncStaged) -> SyncTransition {
        self.apply(staged.0.map(|staged| *staged))
    }

    fn stage_segment(&self, update: ParentGridUpdate) -> Result<Option<Staged>, SyncError> {
        if self.parent == Some(Parent::Segment(update)) {
            return Ok(None);
        }
        self.check_parent_revision(update.parent())?;
        let (grid, descent) = match self.timeline {
            Timeline::Host => {
                let (grid, descent) =
                    self.derived_grid(update.epoch(), update.anchor(), update.meter())?;
                validate_successor(&self.grid, &grid, Withdrawal::Refused)?;
                (grid, Some(descent))
            }
            Timeline::Off | Timeline::Local(_) => (self.grid.clone(), None),
        };
        let takeover = Takeover {
            commit: Some(update.anchor().frame()),
            next_operation: self.next_operation,
        };
        self.stage(
            grid,
            self.timeline,
            Some(Parent::Segment(update)),
            descent,
            takeover,
        )
        .map(Some)
    }

    /// A group in [`crate::SyncMode::HostSync`] withdraws its grid with its
    /// parent's, and releases its sounding members when the parent did.
    fn stage_withdrawal(&self, withdrawal: ParentWithdrawal) -> Result<Option<Staged>, SyncError> {
        let parent = Some(Parent::Withdrawn(withdrawal.parent()));
        if self.parent == parent {
            return Ok(None);
        }
        self.check_parent_revision(withdrawal.parent())?;
        let at = withdrawal.at();
        if !matches!(self.timeline, Timeline::Host) {
            let takeover = Takeover {
                commit: Some(at),
                next_operation: self.next_operation,
            };
            return self
                .stage(self.grid.clone(), self.timeline, parent, None, takeover)
                .map(Some);
        }
        let grid = self.withdrawn_grid()?;
        validate_successor(&self.grid, &grid, Withdrawal::Allowed)?;
        let mut next_operation = self.next_operation;
        let release = withdrawal
            .release()
            .map(|transport| {
                take_operation(self.grid.id(), &mut next_operation)
                    .map(|operation| (operation, transport))
            })
            .transpose()?;
        let descent = ParentWithdrawal::new(grid.stamp(), at, withdrawal.release());
        let takeover = Takeover {
            commit: Some(at),
            next_operation,
        };
        let mut staged = self.stage(
            grid,
            Timeline::Host,
            parent,
            Some(ParentFact::Withdrawn(descent)),
            takeover,
        )?;
        if let Some((operation, transport)) = release {
            staged.pending = self.handoffs(&staged.grid, operation, transport, at)?;
        }
        Ok(Some(staged))
    }

    fn stage_axis(&self, update: SessionAxisUpdate) -> Result<Option<Staged>, SyncError> {
        let axis = MapAxis::Session(update.axis());
        if axis == self.grid.axis() {
            return Ok(None);
        }
        let grid = BeatGridSnapshot::unavailable(self.grid.id(), self.next_revision()?, axis);
        validate_successor(&self.grid, &grid, Withdrawal::Refused)?;
        self.stage(
            grid,
            self.timeline.on_new_axis(),
            None,
            Some(ParentFact::Axis(update)),
            Takeover {
                commit: None,
                next_operation: self.next_operation,
            },
        )
        .map(Some)
    }

    /// Moves a joining group and its subtree onto the new parent's axis
    /// without an unavailable step in every epoch the parent already passed.
    fn stage_join(&self, update: SessionAxisUpdate) -> Result<Option<Staged>, SyncError> {
        let axis = MapAxis::Session(update.axis());
        if axis == self.grid.axis() {
            return Ok(None);
        }
        let grid = BeatGridSnapshot::unavailable(self.grid.id(), self.next_revision()?, axis);
        self.stage(
            grid,
            self.timeline.on_new_axis(),
            None,
            Some(ParentFact::Joined(update)),
            Takeover {
                commit: None,
                next_operation: self.next_operation,
            },
        )
        .map(Some)
    }

    /// Refuses a parent fact older than the one this group already accepted
    /// from the same parent grid.
    fn check_parent_revision(&self, given: BeatGridStamp) -> Result<(), SyncError> {
        match self.parent.map(BeatGridStamp::from) {
            Some(current)
                if current.grid_id() == given.grid_id()
                    && given.revision() <= current.revision() =>
            {
                Err(SyncError::StaleGridRevision { current, given })
            }
            _ => Ok(()),
        }
    }

    /// Stages `grid` under `timeline` as this group's successor, carrying
    /// every member decision and applied map onto it, and stages `descent` on
    /// every direct child group.
    pub(super) fn stage(
        &self,
        grid: BeatGridSnapshot,
        timeline: Timeline,
        parent: Option<Parent>,
        descent: Option<ParentFact>,
        takeover: Takeover,
    ) -> Result<Staged, SyncError> {
        let Refreshed {
            pending,
            applied,
            next_map,
            next_operation,
        } = self.refreshed(&grid, timeline, takeover)?;
        let children = match descent {
            Some(fact) => self
                .members
                .iter()
                .filter_map(|member| match member {
                    SyncMember::Group { group, .. } => Some(group.stage_fact(fact)),
                    SyncMember::Grid { .. } => None,
                })
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };
        let mut staged = Staged {
            grid,
            timeline,
            before_entry: self.before_entry,
            parent,
            pending,
            applied,
            next_map,
            next_operation,
            children,
        };
        if let Some((operation, prior)) = self.before_entry
            && matches!(staged.timeline, Timeline::Host)
            && staged.grid.axis() == self.grid.axis()
            && self
                .reconcile_before_entry(
                    &staged.grid,
                    staged.timeline,
                    &staged.pending,
                    staged.before_entry,
                )
                .is_none()
            && let Some(member) = self
                .pending
                .iter()
                .find(|held| held.operation() == operation)
        {
            // A successor Host grid pushed an unclaimed entry beyond its
            // finite window. The accepted preparation is withdrawn and the
            // still-sounding timeline wins this transaction.
            let restored_grid = self.restored_entry_grid(prior)?;
            let restored = self.refreshed(&restored_grid, prior.timeline(), takeover)?;
            staged.grid = restored_grid;
            staged.timeline = prior.timeline();
            staged.pending = restored.pending;
            if self.applied_of(member.member()).is_none() {
                staged
                    .pending
                    .retain(|held| held.member() != member.member());
            }
            staged.applied = restored.applied;
            for lane in &mut staged.applied {
                lane.restore_local_lock(prior.grid(), staged.grid.stamp());
            }
            staged.next_map = restored.next_map;
            staged.next_operation = restored.next_operation;
            staged.before_entry = None;
            staged.children.clear();
        }
        Ok(staged)
    }

    /// Commits a change staged on this unchanged subtree, and returns every
    /// preparation it issued and withdrew.
    pub(super) fn apply(&mut self, staged: Option<Staged>) -> SyncTransition {
        let Some(staged) = staged else {
            return SyncTransition::default();
        };
        let mut committed = transition(&self.pending, &staged.pending);
        let before_entry = self.reconcile_before_entry(
            &staged.grid,
            staged.timeline,
            &staged.pending,
            staged.before_entry,
        );
        self.grid = staged.grid;
        self.timeline = staged.timeline;
        self.before_entry = before_entry;
        self.parent = staged.parent;
        self.pending = staged.pending;
        self.applied = staged.applied;
        self.next_map = staged.next_map;
        self.next_operation = staged.next_operation;
        let mut children = staged.children.into_iter();
        for member in &mut self.members {
            if let SyncMember::Group { group, .. } = member
                && let Some(child) = children.next()
            {
                committed.append(group.apply_staged(child));
            }
        }
        committed
    }
}
