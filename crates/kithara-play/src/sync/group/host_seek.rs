use kithara_warp::{
    BeatGridStamp, SyncAdmission, SyncError, SyncMember, SyncOperationId, TopologyStamp,
    TransportRevision, WarpMapRevision,
};

use super::super::{
    GroupState,
    prepare::{PreparedDisposition, PreparedSync, align_member, host_seek_policy},
};

#[derive(Clone)]
pub(crate) struct PreparedHostReconcile {
    alignment: super::super::prepare::MemberAlignment,
    pub(crate) expected_generation: (kithara_warp::LoadGeneration, TransportRevision),
    pub(crate) expected_operation: SyncOperationId,
    pub(crate) expected_warp_map: WarpMapRevision,
    expected_owner_grid: BeatGridStamp,
    expected_member_grid: BeatGridStamp,
    pub(crate) prepared: PreparedSync,
    pub(crate) topology: TopologyStamp,
}

pub(crate) fn prepare<G>(
    group: &GroupState<G>,
    member_stamp: BeatGridStamp,
    source: kithara_warp::AlignmentSource,
    transport: TransportRevision,
) -> Result<PreparedHostReconcile, SyncError>
where
    G: kithara_warp::SyncGroup<NestedGroup = G>,
{
    let target = member_stamp.grid_id();
    let (alignment, member) = group
        .members
        .iter()
        .find_map(|member| match member {
            SyncMember::Grid {
                alignment, grid, ..
            } if grid.id() == target => Some((*alignment, grid.snapshot())),
            SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
        })
        .ok_or_else(|| SyncError::MemberNotFound {
            group_id: group.grid.id(),
            member_id: target,
        })?;
    if member.stamp() != member_stamp {
        return Err(SyncError::OwnerUnavailable);
    }
    let alignment = align_member(
        &group.grid,
        &member,
        alignment,
        source.frontier(),
        source.preparation_source(),
        host_seek_policy(source),
    )
    .map_err(|_| SyncError::MemberNotFound {
        group_id: group.grid.id(),
        member_id: target,
    })?;
    let operation = group
        .next_operation
        .ok_or_else(|| SyncError::OperationIdExhausted {
            group_id: group.grid.id(),
        })?;
    let warp_map =
        group
            .warp_map
            .checked_next()
            .ok_or_else(|| SyncError::WarpMapRevisionExhausted {
                group_id: group.grid.id(),
            })?;
    Ok(PreparedHostReconcile {
        alignment,
        expected_generation: (group.generations.0, transport),
        expected_operation: operation,
        expected_warp_map: group.warp_map,
        expected_owner_grid: group.grid.stamp(),
        expected_member_grid: member_stamp,
        prepared: PreparedSync {
            operation,
            warp_map,
            activation: alignment.activation,
            activation_beat: alignment.activation_beat,
            source: alignment.source,
            target,
            disposition: PreparedDisposition::Lock,
            projection: kithara_warp::BeatGridSnapshot::projection(
                member.clone(),
                group.grid.clone(),
                *alignment.alignment.source().value(),
                alignment.activation,
            )
            .map_err(|reason| SyncError::GridNotProjectable {
                group_id: group.grid.id(),
                member_id: target,
                reason,
            })?,
        },
        topology: TopologyStamp::new(group.grid.id(), group.topology_revision),
    })
}

pub(crate) fn commit<G>(
    group: &mut GroupState<G>,
    candidate: PreparedHostReconcile,
) -> Result<SyncAdmission, SyncError>
where
    G: kithara_warp::SyncGroup<NestedGroup = G>,
{
    if group.generations != candidate.expected_generation
        || group.next_operation != Some(candidate.expected_operation)
        || group.warp_map != candidate.expected_warp_map
        || group.grid.stamp() != candidate.expected_owner_grid
        || TopologyStamp::new(group.grid.id(), group.topology_revision) != candidate.topology
    {
        return Err(SyncError::OwnerUnavailable);
    }
    let (alignment, member) = group
        .members
        .iter_mut()
        .find_map(|member| match member {
            SyncMember::Grid {
                alignment, grid, ..
            } if grid.id() == candidate.prepared.target => Some((alignment, grid.snapshot())),
            SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
        })
        .ok_or_else(|| SyncError::MemberNotFound {
            group_id: group.grid.id(),
            member_id: candidate.prepared.target,
        })?;
    if member.stamp() != candidate.expected_member_grid {
        return Err(SyncError::OwnerUnavailable);
    }
    *alignment = Some(candidate.alignment.alignment);
    group.generations = candidate.expected_generation;
    let admission = SyncAdmission::Prepared {
        operation: candidate.prepared.operation,
        topology: candidate.topology,
        warp_map: candidate.prepared.warp_map,
        activation: candidate.prepared.activation,
    };
    group.warp_map = candidate.prepared.warp_map;
    group.prepared.insert(candidate.prepared);
    group.unavailable = None;
    group.waiting = None;
    group.next_operation = group.next_operation.and_then(SyncOperationId::checked_next);
    Ok(admission)
}
