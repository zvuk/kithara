use std::fmt;

use super::{
    BeatAlignment, SyncError, SyncGroup, SyncGroupTopologyError, SyncMemberKind, SyncMemberSnapshot,
};
use crate::{BeatGrid, BeatGridId, BeatGridSnapshot, BeatGridStamp, MapPoint};

/// Whether a member is sounding or waiting to enter its group.
///
/// Orthogonal to synchronization: a waiting member is pre-aligned so its first
/// downbeat falls on a downbeat of the owner grid, and enters silently until
/// the group arms it. An armed member is audible and follows the owner's tempo
/// and phase.
///
/// Three owners arm a member: the deck arms the track it starts playing, the
/// group arms the member whose prepared entry the renderer locks, and the
/// parent group arms a nested group through [`TopologyOperation::Arm`].
///
/// The state belongs to the edge between a group and one member, so a nested
/// group carries its own arm in its parent independently of how its members
/// are armed inside it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberArm {
    /// Pre-aligned and silent, waiting for its entry frame.
    Waiting,
    /// Sounding and actively synchronized.
    Armed,
}

/// One exclusively owned live grid or statically typed nested synchronization group.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub enum SyncMember<G: SyncGroup> {
    /// A readable live grid.
    Grid {
        /// Alignment from this member to its direct parent, once both grids
        /// expose usable geometry.
        #[field(get, copy)]
        alignment: Option<BeatAlignment>,
        /// Whether this member sounds or waits for its entry frame.
        #[field(get, copy, set)]
        arm: MemberArm,
        /// Live grid handle owned by the parent group.
        grid: Box<dyn BeatGrid + Send + Sync>,
    },
    /// A live nested synchronization group.
    Group {
        /// Alignment from this member to its direct parent, once both grids
        /// expose usable geometry.
        alignment: Option<BeatAlignment>,
        /// Whether this member sounds or waits for its entry frame.
        arm: MemberArm,
        /// Live group owned by the parent group.
        group: Box<G>,
    },
}

impl<G: SyncGroup> fmt::Debug for SyncMember<G> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grid {
                alignment,
                arm,
                grid,
            } => formatter
                .debug_struct("SyncMember::Grid")
                .field("alignment", alignment)
                .field("arm", arm)
                .field("grid_id", &grid.id())
                .finish(),
            Self::Group {
                alignment,
                arm,
                group,
            } => formatter
                .debug_struct("SyncMember::Group")
                .field("alignment", alignment)
                .field("arm", arm)
                .field("group_id", &group.id())
                .finish(),
        }
    }
}

impl<G: SyncGroup> SyncMember<G> {
    /// Returns the stable identity of this live member.
    #[must_use]
    pub fn id(&self) -> BeatGridId {
        self.into()
    }

    /// Returns whether this member is an ordinary grid or a nested group.
    #[must_use]
    pub const fn kind(&self) -> SyncMemberKind {
        match self {
            Self::Grid { .. } => SyncMemberKind::Grid,
            Self::Group { .. } => SyncMemberKind::Group,
        }
    }

    /// Materializes the current member and restamps an established alignment
    /// to the current child and parent revisions.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError`] for an identity or nested-topology violation.
    pub fn snapshot_for(&self, parent: &BeatGridSnapshot) -> Result<SyncMemberSnapshot, SyncError> {
        match self {
            Self::Grid {
                alignment,
                arm,
                grid,
            } => {
                let expected = grid.id();
                let grid = grid.snapshot();
                if grid.id() != expected {
                    return Err(SyncError::GridIdentityMismatch {
                        expected,
                        given: grid.id(),
                    });
                }
                let alignment = restamp_alignment(*alignment, grid.stamp(), parent.stamp())?;
                Ok(SyncMemberSnapshot::new_grid(grid, alignment, *arm))
            }
            Self::Group {
                alignment,
                arm,
                group,
            } => {
                let expected = group.id();
                let group = group.topology()?;
                if group.stamp().group_id() != expected {
                    return Err(SyncError::GridIdentityMismatch {
                        expected,
                        given: group.stamp().group_id(),
                    });
                }
                let alignment =
                    restamp_alignment(*alignment, group.group_grid().stamp(), parent.stamp())?;
                Ok(SyncMemberSnapshot::new_group(group, alignment, *arm))
            }
        }
    }
}

impl<G: SyncGroup> From<&SyncMember<G>> for BeatGridId {
    fn from(member: &SyncMember<G>) -> Self {
        match member {
            SyncMember::Grid { grid, .. } => grid.id(),
            SyncMember::Group { group, .. } => group.id(),
        }
    }
}

fn restamp_alignment(
    alignment: Option<BeatAlignment>,
    source: BeatGridStamp,
    target: BeatGridStamp,
) -> Result<Option<BeatAlignment>, SyncError> {
    alignment
        .map(|alignment| {
            let given_source = alignment.source().stamp();
            if given_source.grid_id() != source.grid_id() {
                return Err(SyncGroupTopologyError::StaleSourceAlignment {
                    expected: source,
                    given: given_source,
                }
                .into());
            }
            let given_target = alignment.target().stamp();
            if given_target.grid_id() != target.grid_id() {
                return Err(SyncGroupTopologyError::StaleTargetAlignment {
                    expected: target,
                    given: given_target,
                }
                .into());
            }
            Ok(BeatAlignment::new(
                MapPoint::new(source, *alignment.source().value()),
                MapPoint::new(target, *alignment.target().value()),
            ))
        })
        .transpose()
}
