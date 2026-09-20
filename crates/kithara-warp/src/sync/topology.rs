use std::collections::BTreeSet;

use kithara_platform::sync::Arc;

use super::{BeatAlignment, MemberArm, TopologyRevision, TopologyStamp};
use crate::{BeatGridId, BeatGridSnapshot, BeatGridStamp};

/// One immutable observation of a direct live member.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncMemberSnapshot {
    /// Returns the direct member's frozen grid.
    #[field(get)]
    grid: BeatGridSnapshot,
    /// Returns the alignment edge from the direct parent.
    #[field(get, copy)]
    alignment: Option<BeatAlignment>,
    /// Returns whether the member sounds or waits for its entry frame.
    #[field(get, copy)]
    arm: MemberArm,
    /// Returns the frozen nested topology when this member is a group.
    #[field(get = group_topology)]
    group: Option<SyncGroupSnapshot>,
}

impl SyncMemberSnapshot {
    /// Freezes one ordinary grid edge, including a pending edge without alignment.
    #[must_use]
    pub const fn new_grid(
        grid: BeatGridSnapshot,
        alignment: Option<BeatAlignment>,
        arm: MemberArm,
    ) -> Self {
        Self {
            alignment,
            arm,
            grid,
            group: None,
        }
    }

    /// Freezes one nested group edge, including a pending edge without alignment.
    #[must_use]
    pub fn new_group(
        group: SyncGroupSnapshot,
        alignment: Option<BeatAlignment>,
        arm: MemberArm,
    ) -> Self {
        Self {
            alignment,
            arm,
            grid: group.group_grid.clone(),
            group: Some(group),
        }
    }

    /// Returns this edge with the member marked sounding or waiting.
    #[must_use]
    pub fn with_arm(self, arm: MemberArm) -> Self {
        Self { arm, ..self }
    }
}

/// One immutable observation of a synchronization group's grid and members.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SyncGroupSnapshot {
    /// Returns the direct members frozen into this topology revision.
    #[field(get)]
    members: Arc<[SyncMemberSnapshot]>,
    /// Returns the group's authoritative musical-coordinate snapshot.
    #[field(get)]
    group_grid: BeatGridSnapshot,
    /// Returns the topology identity and revision.
    #[field(get, copy)]
    stamp: TopologyStamp,
}

impl SyncGroupSnapshot {
    /// Creates and validates a topology tree.
    ///
    /// # Errors
    ///
    /// Returns [`SyncGroupTopologyError`] when an edge uses stale grid stamps,
    /// repeats a member, or makes the tree recursive.
    pub fn try_new<I>(
        group_grid: BeatGridSnapshot,
        revision: TopologyRevision,
        members: I,
    ) -> Result<Self, SyncGroupTopologyError>
    where
        I: IntoIterator<Item = SyncMemberSnapshot>,
    {
        let members: Arc<[SyncMemberSnapshot]> = members.into_iter().collect();
        validate_edges(&group_grid, &members)?;
        validate_tree(group_grid.id(), &members)?;
        Ok(Self {
            stamp: TopologyStamp::new(group_grid.id(), revision),
            group_grid,
            members,
        })
    }
}

/// A synchronization topology violates tree, identity, or revision rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncGroupTopologyError {
    /// A direct ordinary member is the group grid itself.
    #[error("group grid {group_id} cannot be its own member")]
    SelfMember { group_id: BeatGridId },
    /// A nested path returns to an ancestor group.
    #[error("nested group path returns to ancestor grid {group_id}")]
    Cycle { group_id: BeatGridId },
    /// One nested group appears below more than one parent edge.
    #[error("nested group {group_id} has multiple parent edges")]
    MultipleParents { group_id: BeatGridId },
    /// One ordinary grid appears more than once in the same topology tree.
    #[error("leaf grid {member_id} appears more than once")]
    DuplicateLeaf { member_id: BeatGridId },
    /// One grid identity appears as both a nested group and an ordinary leaf.
    #[error("grid {member_id} appears as both a group and a leaf")]
    ConflictingMemberKind { member_id: BeatGridId },
    /// The alignment's target point belongs to another grid revision.
    #[error("alignment target stamp is {given:?}, expected {expected:?}")]
    StaleTargetAlignment {
        expected: BeatGridStamp,
        given: BeatGridStamp,
    },
    /// The alignment's source point belongs to another grid revision.
    #[error("alignment source stamp is {given:?}, expected {expected:?}")]
    StaleSourceAlignment {
        expected: BeatGridStamp,
        given: BeatGridStamp,
    },
}

fn validate_edges(
    group_grid: &BeatGridSnapshot,
    members: &[SyncMemberSnapshot],
) -> Result<(), SyncGroupTopologyError> {
    for member in members {
        if let Some(alignment) = member.alignment {
            let target_stamp = alignment.target().stamp();
            if target_stamp != group_grid.stamp() {
                return Err(SyncGroupTopologyError::StaleTargetAlignment {
                    expected: group_grid.stamp(),
                    given: target_stamp,
                });
            }
            let source_stamp = alignment.source().stamp();
            if source_stamp != member.grid.stamp() {
                return Err(SyncGroupTopologyError::StaleSourceAlignment {
                    expected: member.grid.stamp(),
                    given: source_stamp,
                });
            }
        }
    }
    Ok(())
}

fn validate_tree(
    root: BeatGridId,
    members: &[SyncMemberSnapshot],
) -> Result<(), SyncGroupTopologyError> {
    let mut groups = BTreeSet::from([root]);
    let mut leaves = BTreeSet::new();
    for member in members {
        match member.group_topology() {
            Some(group) => visit_group(group, root, &mut groups, &mut leaves)?,
            None if member.grid.id() == root => {
                return Err(SyncGroupTopologyError::SelfMember { group_id: root });
            }
            None => visit_leaf(member.grid.id(), &groups, &mut leaves)?,
        }
    }
    Ok(())
}

fn visit_group(
    group: &SyncGroupSnapshot,
    root: BeatGridId,
    groups: &mut BTreeSet<BeatGridId>,
    leaves: &mut BTreeSet<BeatGridId>,
) -> Result<(), SyncGroupTopologyError> {
    let group_id = group.group_grid.id();
    if group_id == root {
        return Err(SyncGroupTopologyError::Cycle { group_id });
    }
    if leaves.contains(&group_id) {
        return Err(SyncGroupTopologyError::ConflictingMemberKind {
            member_id: group_id,
        });
    }
    if !groups.insert(group_id) {
        return Err(SyncGroupTopologyError::MultipleParents { group_id });
    }
    for member in group.members() {
        match member.group_topology() {
            Some(child) => visit_group(child, root, groups, leaves)?,
            None if member.grid.id() == root => {
                return Err(SyncGroupTopologyError::Cycle { group_id: root });
            }
            None => visit_leaf(member.grid.id(), groups, leaves)?,
        }
    }
    Ok(())
}

fn visit_leaf(
    member_id: BeatGridId,
    groups: &BTreeSet<BeatGridId>,
    leaves: &mut BTreeSet<BeatGridId>,
) -> Result<(), SyncGroupTopologyError> {
    if groups.contains(&member_id) {
        return Err(SyncGroupTopologyError::ConflictingMemberKind { member_id });
    }
    if !leaves.insert(member_id) {
        return Err(SyncGroupTopologyError::DuplicateLeaf { member_id });
    }
    Ok(())
}
