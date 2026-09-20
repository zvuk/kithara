use kithara_warp::{
    BeatGridId, BeatGridSnapshot, MemberArm, SyncError, SyncGroup, SyncGroupSnapshot,
    SyncGroupTopologyError, SyncMember, SyncMemberKind, SyncMemberSnapshot, TopologyOperation,
    TopologyRevision, TopologyStamp,
};

pub(super) fn materialize_topology<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    revision: TopologyRevision,
    members: &[SyncMember<G>],
) -> Result<SyncGroupSnapshot, SyncError> {
    let members = members
        .iter()
        .map(|member| member.snapshot_for(grid))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncGroupSnapshot::try_new(grid.clone(), revision, members)?)
}

pub(super) fn validate_topology_candidate<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    revision: TopologyRevision,
    members: &[SyncMember<G>],
    operations: &[TopologyOperation<G>],
    member_kind: SyncMemberKind,
) -> Result<(), SyncError> {
    let mut candidate: Vec<SyncMemberSnapshot> = members
        .iter()
        .map(|member| member.snapshot_for(grid))
        .collect::<Result<_, _>>()?;

    apply_snapshot_operations(grid, &mut candidate, operations, member_kind)?;

    SyncGroupSnapshot::try_new(grid.clone(), revision, candidate)?;
    Ok(())
}

fn validate_incoming_member<G: SyncGroup<NestedGroup = G>>(
    parent: &BeatGridSnapshot,
    member: &SyncMember<G>,
    expected: SyncMemberKind,
) -> Result<SyncMemberSnapshot, SyncError> {
    let given = member.kind();
    if given != expected {
        return Err(SyncError::InvalidMemberKind {
            expected,
            given,
            group_id: parent.id(),
            member_id: member.id(),
        });
    }
    let snapshot = member.snapshot_for(parent)?;
    if let Some(alignment) = member.alignment() {
        if alignment.target().stamp() != parent.stamp() {
            return Err(SyncGroupTopologyError::StaleTargetAlignment {
                expected: parent.stamp(),
                given: alignment.target().stamp(),
            }
            .into());
        }
        if alignment.source().stamp() != snapshot.grid().stamp() {
            return Err(SyncGroupTopologyError::StaleSourceAlignment {
                expected: snapshot.grid().stamp(),
                given: alignment.source().stamp(),
            }
            .into());
        }
    }
    Ok(snapshot)
}

fn member_index(
    group_id: BeatGridId,
    members: &[SyncMemberSnapshot],
    member_id: BeatGridId,
) -> Result<usize, SyncError> {
    members
        .iter()
        .position(|member| member.grid().id() == member_id)
        .ok_or(SyncError::MemberNotFound {
            group_id,
            member_id,
        })
}

fn apply_snapshot_operations<G: SyncGroup<NestedGroup = G>>(
    parent: &BeatGridSnapshot,
    members: &mut Vec<SyncMemberSnapshot>,
    operations: &[TopologyOperation<G>],
    member_kind: SyncMemberKind,
) -> Result<(), SyncError> {
    for operation in operations {
        match operation {
            TopologyOperation::Attach { member } => {
                members.push(validate_incoming_member(parent, member, member_kind)?);
            }
            TopologyOperation::Detach { member } => {
                let index = member_index(parent.id(), members, *member)?;
                members.remove(index);
            }
            TopologyOperation::Arm { member } => {
                let index = member_index(parent.id(), members, *member)?;
                members[index] = members[index].clone().with_arm(MemberArm::Armed);
            }
            TopologyOperation::Disarm { member } => {
                let index = member_index(parent.id(), members, *member)?;
                members[index] = members[index].clone().with_arm(MemberArm::Waiting);
            }
            TopologyOperation::Replace {
                member,
                replacement,
            } => {
                let index = member_index(parent.id(), members, *member)?;
                let arm = members[index].arm();
                members[index] =
                    validate_incoming_member(parent, replacement, member_kind)?.with_arm(arm);
            }
        }
    }
    Ok(())
}

pub(super) fn apply_topology_operations<G: SyncGroup<NestedGroup = G>>(
    members: &mut Vec<SyncMember<G>>,
    operations: Box<[TopologyOperation<G>]>,
) {
    for operation in Vec::from(operations) {
        match operation {
            TopologyOperation::Attach { member } => members.push(member),
            TopologyOperation::Detach { member } => {
                members.retain(|candidate| candidate.id() != member);
            }
            TopologyOperation::Arm { member } => {
                set_member_arm(members, member, MemberArm::Armed);
            }
            TopologyOperation::Disarm { member } => {
                set_member_arm(members, member, MemberArm::Waiting);
            }
            TopologyOperation::Replace {
                member,
                replacement,
            } => {
                let mut replacement = Some(replacement);
                for candidate in members.iter_mut() {
                    if candidate.id() == member
                        && let Some(mut replacement) = replacement.take()
                    {
                        replacement.set_arm(candidate.arm());
                        *candidate = replacement;
                    }
                }
            }
        }
    }
}

/// Marks one live member sounding or waiting.
///
/// The snapshot preview rejects an operation naming an absent member before
/// this runs, so a missing target here cannot reach a committed transaction.
fn set_member_arm<G: SyncGroup<NestedGroup = G>>(
    members: &mut [SyncMember<G>],
    target: BeatGridId,
    arm: MemberArm,
) {
    if let Some(member) = members.iter_mut().find(|member| member.id() == target) {
        member.set_arm(arm);
    }
}

pub(super) fn preview_topology<G: SyncGroup<NestedGroup = G>>(
    topology: &SyncGroupSnapshot,
    base: TopologyStamp,
    operations: &[TopologyOperation<G>],
    member_kind: SyncMemberKind,
) -> Result<(SyncGroupSnapshot, bool), SyncError> {
    if topology.stamp().group_id() == base.group_id() {
        if topology.stamp() != base {
            return Err(SyncError::StaleTopology {
                expected: topology.stamp(),
                given: base,
            });
        }
        if operations.is_empty() {
            return Ok((topology.clone(), false));
        }

        let revision = next_topology_revision(base.group_id(), base.revision())?;
        let mut members = topology.members().to_vec();
        apply_snapshot_operations(topology.group_grid(), &mut members, operations, member_kind)?;
        let candidate =
            SyncGroupSnapshot::try_new(topology.group_grid().clone(), revision, members)?;
        return Ok((candidate, true));
    }

    let target = base.group_id();
    let index = topology
        .members()
        .iter()
        .position(|member| {
            member.group_topology().is_some_and(|group| {
                group.stamp().group_id() == target || topology_contains(group, target)
            })
        })
        .ok_or(SyncError::GroupNotFound { group_id: target })?;
    let member = &topology.members()[index];
    let child = member
        .group_topology()
        .ok_or(SyncError::GroupNotFound { group_id: target })?;
    let (child, changed) = preview_topology(child, base, operations, SyncMemberKind::Grid)?;
    if !changed {
        return Ok((topology.clone(), false));
    }

    let revision =
        next_topology_revision(topology.stamp().group_id(), topology.stamp().revision())?;
    let mut members = topology.members().to_vec();
    members[index] = SyncMemberSnapshot::new_group(child, member.alignment(), member.arm());
    let candidate = SyncGroupSnapshot::try_new(topology.group_grid().clone(), revision, members)?;
    Ok((candidate, true))
}

pub(super) fn owns_direct_grid<G: SyncGroup<NestedGroup = G>>(
    members: &[SyncMember<G>],
    target: BeatGridId,
) -> bool {
    members
        .iter()
        .any(|member| matches!(member, SyncMember::Grid { grid, .. } if grid.id() == target))
}

pub(super) fn routed_group<G: SyncGroup<NestedGroup = G>>(
    members: &mut [SyncMember<G>],
    target: BeatGridId,
) -> Result<Option<&mut G>, SyncError> {
    for member in members {
        let SyncMember::Group { group, .. } = member else {
            continue;
        };
        let group_id = group.id();
        let topology = group.topology()?;
        if topology.stamp().group_id() != group_id {
            return Err(SyncError::GridIdentityMismatch {
                expected: group_id,
                given: topology.stamp().group_id(),
            });
        }
        if group_id == target || topology_contains(&topology, target) {
            return Ok(Some(group.as_mut()));
        }
    }
    Ok(None)
}

fn topology_contains(topology: &SyncGroupSnapshot, target: BeatGridId) -> bool {
    topology.members().iter().any(|member| {
        member.grid().id() == target
            || member
                .group_topology()
                .is_some_and(|group| topology_contains(group, target))
    })
}

pub(super) fn next_topology_revision(
    group_id: BeatGridId,
    current: TopologyRevision,
) -> Result<TopologyRevision, SyncError> {
    current
        .checked_next()
        .ok_or(SyncError::TopologyRevisionExhausted { group_id })
}
