use kithara_warp::{
    BeatGridId, BeatGridSnapshot, BeatsPerMinute, LoadGeneration, MapPosition, MapRegion,
    SessionFrame, SyncAdmission, SyncApplied, SyncCapability, SyncError, SyncGroup, SyncIntent,
    SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncOperationId, SyncRejected,
    SyncStatusSnapshot, TopologyRevision, TopologyStamp, TransportRevision, WarpMapRevision,
};

use super::{
    TempoSource,
    prepare::{PreparedSync, align_member},
    topology::{
        apply_topology_operations, materialize_topology, next_topology_revision, owns_direct_grid,
        preview_topology, routed_group, validate_topology_candidate,
    },
};

/// The mutable slots of one group that a transaction may write.
pub(super) struct GroupSlots<'a, G: SyncGroup<NestedGroup = G>> {
    pub(super) next_operation: &'a mut Option<SyncOperationId>,
    pub(super) unavailable: &'a mut Option<(SyncOperationId, SyncCapability)>,
    pub(super) waiting: &'a mut Option<(SyncOperationId, MapRegion)>,
    pub(super) mode: &'a mut SyncMode,
    pub(super) tempo: &'a mut TempoSource,
    pub(super) generations: &'a mut (LoadGeneration, TransportRevision),
    pub(super) warp_map: &'a mut WarpMapRevision,
    pub(super) prepared: &'a mut Option<PreparedSync>,
    pub(super) locked: &'a mut Option<SyncApplied>,
    pub(super) topology_revision: &'a mut TopologyRevision,
    pub(super) members: &'a mut Vec<SyncMember<G>>,
}

/// The slots that decide which status one group reports.
pub(super) struct StatusSlots {
    pub(super) unavailable: Option<(SyncOperationId, SyncCapability)>,
    pub(super) waiting: Option<(SyncOperationId, MapRegion)>,
    pub(super) prepared: Option<PreparedSync>,
    pub(super) locked: Option<SyncApplied>,
}

pub(super) fn status(topology: TopologyStamp, slots: &StatusSlots) -> SyncStatusSnapshot {
    if let Some((operation, required)) = slots.waiting {
        return SyncStatusSnapshot::WaitingForGrid {
            operation,
            topology,
            required,
        };
    }
    if let Some(prepared) = slots.prepared {
        return SyncStatusSnapshot::Prepared {
            operation: prepared.operation,
            topology,
            warp_map: prepared.warp_map,
            activation: prepared.activation,
        };
    }
    if let Some(applied) = slots.locked {
        return SyncStatusSnapshot::Locked {
            applied,
            phase_error_frames: 0.0,
        };
    }
    slots.unavailable.map_or(
        SyncStatusSnapshot::Off { topology },
        |(operation, capability)| SyncStatusSnapshot::Unavailable {
            operation,
            topology,
            capability,
        },
    )
}

pub(super) fn transact<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    slots: GroupSlots<'_, G>,
    member_kind: SyncMemberKind,
    seed: Option<BeatsPerMinute>,
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    let target = operation.target();
    let topology_operation = matches!(&operation, SyncOperation::Topology { .. });
    if target == grid.id() || (!topology_operation && owns_direct_grid(slots.members, target)) {
        return transact_local(grid, slots, member_kind, seed, operation);
    }

    let topology_change = matches!(
        &operation,
        SyncOperation::Topology { operations, .. } if !operations.is_empty()
    );
    if let SyncOperation::Topology { base, operations } = &operation {
        let root = match materialize_topology(grid, *slots.topology_revision, slots.members) {
            Ok(root) => root,
            Err(error) => return Err(SyncRejected::new(error, operation)),
        };
        if let Err(error) = preview_topology(&root, *base, operations, member_kind) {
            return Err(SyncRejected::new(error, operation));
        }
    }
    let parent_revision = match topology_change
        .then(|| next_topology_revision(grid.id(), *slots.topology_revision))
        .transpose()
    {
        Ok(revision) => revision,
        Err(error) => return Err(SyncRejected::new(error, operation)),
    };
    let group = match routed_group(slots.members, target) {
        Ok(Some(group)) => group,
        Ok(None) => {
            return Err(SyncRejected::new(
                SyncError::GroupNotFound { group_id: target },
                operation,
            ));
        }
        Err(error) => return Err(SyncRejected::new(error, operation)),
    };
    let admission = group.transact(operation)?;
    if matches!(admission, SyncAdmission::TopologyChanged { .. })
        && let Some(revision) = parent_revision
    {
        *slots.topology_revision = revision;
    }
    Ok(admission)
}

fn transact_local<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    mut slots: GroupSlots<'_, G>,
    member_kind: SyncMemberKind,
    seed: Option<BeatsPerMinute>,
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    match &operation {
        SyncOperation::Topology { .. } => transact_topology(
            grid,
            slots.topology_revision,
            slots.members,
            slots.next_operation,
            member_kind,
            operation,
        ),
        SyncOperation::Sync { target, .. } if *target != grid.id() => Err(SyncRejected::new(
            SyncError::CapabilityUnavailable {
                capability: SyncCapability::Alignment,
            },
            operation,
        )),
        SyncOperation::Sync { intent, .. } => match intent {
            SyncIntent::Enable => state_changed(
                grid,
                &mut slots,
                (SyncMode::HostSync, TempoSource::Inherited),
                operation,
            ),
            SyncIntent::Disable => match seed {
                Some(tempo) => state_changed(
                    grid,
                    &mut slots,
                    (SyncMode::LocalSync, TempoSource::Local(tempo)),
                    operation,
                ),
                None => deferred(
                    grid,
                    &mut slots,
                    operation,
                    MapRegion::point(MapPosition::Session(SessionFrame::new(0))),
                ),
            },
            SyncIntent::Free => state_changed(
                grid,
                &mut slots,
                (SyncMode::Off, TempoSource::Inherited),
                operation,
            ),
            _ => preserve_rejected(
                unavailable_admission(
                    grid.id(),
                    *slots.topology_revision,
                    slots.next_operation,
                    slots.unavailable,
                    SyncCapability::Alignment,
                ),
                operation,
            ),
        },
        SyncOperation::Transport {
            load, transport, ..
        } => {
            let load = *load;
            let transport = *transport;
            let operation_id = match take_operation(grid.id(), slots.next_operation) {
                Ok(operation_id) => operation_id,
                Err(error) => return Err(SyncRejected::new(error, operation)),
            };
            *slots.unavailable = None;
            *slots.generations = (load, transport);
            Ok(SyncAdmission::Accepted {
                load,
                transport,
                operation: operation_id,
                topology: TopologyStamp::new(grid.id(), *slots.topology_revision),
            })
        }
        SyncOperation::Tempo { target, .. } if *target != grid.id() => Err(SyncRejected::new(
            SyncError::CapabilityUnavailable {
                capability: SyncCapability::Transport,
            },
            operation,
        )),
        SyncOperation::Tempo { tempo, .. } => match *slots.mode {
            SyncMode::HostSync => Err(SyncRejected::new(
                SyncError::TempoInherited { owner: grid.id() },
                operation,
            )),
            SyncMode::LocalSync => {
                let state = (SyncMode::LocalSync, TempoSource::Local(*tempo));
                state_changed(grid, &mut slots, state, operation)
            }
            SyncMode::Off => Err(SyncRejected::new(
                SyncError::CapabilityUnavailable {
                    capability: SyncCapability::Transport,
                },
                operation,
            )),
        },
        SyncOperation::Reconcile { .. } if *slots.mode == SyncMode::Off => preserve_rejected(
            unavailable_admission(
                grid.id(),
                *slots.topology_revision,
                slots.next_operation,
                slots.unavailable,
                SyncCapability::Reconciliation,
            ),
            operation,
        ),
        SyncOperation::Reconcile { .. } => reconcile(grid, slots, operation),
    }
}

fn reconcile<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    mut slots: GroupSlots<'_, G>,
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    let SyncOperation::Reconcile {
        target, frontier, ..
    } = &operation
    else {
        return Err(SyncRejected::new(
            SyncError::CapabilityUnavailable {
                capability: SyncCapability::Reconciliation,
            },
            operation,
        ));
    };
    let Some(member) = slots.members.iter_mut().find_map(|member| match member {
        SyncMember::Grid { alignment, grid } if grid.id() == *target => {
            Some((alignment, grid.snapshot()))
        }
        SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
    }) else {
        return Err(SyncRejected::new(
            SyncError::MemberNotFound {
                group_id: grid.id(),
                member_id: *target,
            },
            operation,
        ));
    };
    let (alignment_slot, member_grid) = member;
    let aligned = match align_member(grid, &member_grid, *frontier) {
        Ok(aligned) => aligned,
        Err(required) => return deferred(grid, &mut slots, operation, required),
    };
    let Some(warp_map) = slots.warp_map.checked_next() else {
        return Err(SyncRejected::new(
            SyncError::WarpMapRevisionExhausted {
                group_id: grid.id(),
            },
            operation,
        ));
    };
    let operation_id = match take_operation(grid.id(), slots.next_operation) {
        Ok(operation_id) => operation_id,
        Err(error) => return Err(SyncRejected::new(error, operation)),
    };
    *alignment_slot = Some(aligned.alignment);
    *slots.warp_map = warp_map;
    *slots.unavailable = None;
    *slots.waiting = None;
    *slots.prepared = Some(PreparedSync {
        operation: operation_id,
        warp_map,
        activation: aligned.activation,
    });
    Ok(SyncAdmission::Prepared {
        operation: operation_id,
        topology: TopologyStamp::new(grid.id(), *slots.topology_revision),
        warp_map,
        activation: aligned.activation,
    })
}

fn state_changed<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    slots: &mut GroupSlots<'_, G>,
    state: (SyncMode, TempoSource),
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    let operation_id = match take_operation(grid.id(), slots.next_operation) {
        Ok(operation_id) => operation_id,
        Err(error) => return Err(SyncRejected::new(error, operation)),
    };
    (*slots.mode, *slots.tempo) = state;
    *slots.unavailable = None;
    *slots.waiting = None;
    *slots.prepared = None;
    *slots.locked = None;
    Ok(SyncAdmission::StateChanged {
        operation: operation_id,
        topology: TopologyStamp::new(grid.id(), *slots.topology_revision),
    })
}

fn deferred<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    slots: &mut GroupSlots<'_, G>,
    operation: SyncOperation<G>,
    required: MapRegion,
) -> Result<SyncAdmission, SyncRejected<G>> {
    let operation_id = match take_operation(grid.id(), slots.next_operation) {
        Ok(operation_id) => operation_id,
        Err(error) => return Err(SyncRejected::new(error, operation)),
    };
    *slots.unavailable = None;
    *slots.waiting = Some((operation_id, required));
    Ok(SyncAdmission::Deferred {
        required,
        operation: operation_id,
        topology: TopologyStamp::new(grid.id(), *slots.topology_revision),
    })
}

fn preserve_rejected<G: SyncGroup<NestedGroup = G>>(
    result: Result<SyncAdmission, SyncError>,
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    result.map_err(|error| SyncRejected::new(error, operation))
}

fn unavailable_admission(
    group_id: BeatGridId,
    topology_revision: TopologyRevision,
    next_operation: &mut Option<SyncOperationId>,
    unavailable: &mut Option<(SyncOperationId, SyncCapability)>,
    capability: SyncCapability,
) -> Result<SyncAdmission, SyncError> {
    let operation = take_operation(group_id, next_operation)?;
    let topology = TopologyStamp::new(group_id, topology_revision);
    *unavailable = Some((operation, capability));
    Ok(SyncAdmission::Unavailable {
        operation,
        topology,
        capability,
    })
}

fn transact_topology<G: SyncGroup<NestedGroup = G>>(
    grid: &BeatGridSnapshot,
    topology_revision: &mut TopologyRevision,
    members: &mut Vec<SyncMember<G>>,
    next_operation: &mut Option<SyncOperationId>,
    member_kind: SyncMemberKind,
    operation: SyncOperation<G>,
) -> Result<SyncAdmission, SyncRejected<G>> {
    let (base, operations) = match operation {
        SyncOperation::Topology { base, operations } => (base, operations),
        operation => {
            return Err(SyncRejected::new(
                SyncError::CapabilityUnavailable {
                    capability: SyncCapability::Topology,
                },
                operation,
            ));
        }
    };
    let reject =
        |error, operations| SyncRejected::new(error, SyncOperation::Topology { base, operations });
    let expected = TopologyStamp::new(grid.id(), *topology_revision);
    if base != expected {
        return Err(reject(
            SyncError::StaleTopology {
                expected,
                given: base,
            },
            operations,
        ));
    }

    let operation_id = match (*next_operation).ok_or_else(|| SyncError::OperationIdExhausted {
        group_id: grid.id(),
    }) {
        Ok(operation_id) => operation_id,
        Err(error) => return Err(reject(error, operations)),
    };
    if operations.is_empty() {
        advance_operation(next_operation);
        return Ok(SyncAdmission::Unchanged {
            operation: operation_id,
            topology: expected,
        });
    }

    let revision = match next_topology_revision(grid.id(), *topology_revision) {
        Ok(revision) => revision,
        Err(error) => return Err(reject(error, operations)),
    };
    if let Err(error) =
        validate_topology_candidate(grid, revision, members, &operations, member_kind)
    {
        return Err(reject(error, operations));
    }
    apply_topology_operations(members, operations);
    *topology_revision = revision;
    advance_operation(next_operation);
    Ok(SyncAdmission::TopologyChanged {
        operation: operation_id,
        topology: TopologyStamp::new(grid.id(), revision),
    })
}

fn take_operation(
    group_id: BeatGridId,
    next: &mut Option<SyncOperationId>,
) -> Result<SyncOperationId, SyncError> {
    let operation = (*next).ok_or(SyncError::OperationIdExhausted { group_id })?;
    advance_operation(next);
    Ok(operation)
}

fn advance_operation(next: &mut Option<SyncOperationId>) {
    *next = next.and_then(SyncOperationId::checked_next);
}
