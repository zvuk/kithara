use kithara_warp::{BeatGridId, MapAxis};

use super::{
    mutation::{
        apply_topology_operations, materialize_topology, next_topology_revision, owns_direct_grid,
        preview_topology, routed_group, validate_topology_candidate,
    },
    preparation::PrepareRequest,
    relocation::RelocateRequest,
    state::GroupState,
};
use crate::{
    ParentFact, SessionAxisUpdate, SyncAdmission, SyncCapability, SyncError, SyncGroup, SyncMember,
    SyncOperation, SyncOperationId, SyncRejected, SyncStaged, TopologyOperation, TopologyStamp,
    TransportOperation,
};

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Routes one operation to this group, one of its direct grids, or the
    /// nested group that owns its target.
    pub(super) fn route(
        &mut self,
        operation: SyncOperation<G>,
    ) -> Result<SyncAdmission, SyncRejected<G>> {
        let target = operation.target();
        let topology_operation = matches!(&operation, SyncOperation::Topology { .. });
        if target == self.grid.id() {
            return self.transact_local(operation);
        }
        if !topology_operation && owns_direct_grid(&self.members, target) {
            return self.transact_member(operation);
        }

        let topology_change = matches!(
            &operation,
            SyncOperation::Topology { operations, .. } if !operations.is_empty()
        );
        if let SyncOperation::Topology { base, operations } = &operation {
            let root = match materialize_topology(&self.grid, self.topology_revision, &self.members)
            {
                Ok(root) => root,
                Err(error) => return Err(SyncRejected::new(error, operation)),
            };
            if let Err(error) = preview_topology(&root, *base, operations, self.member_kind) {
                return Err(SyncRejected::new(error, operation));
            }
        }
        let parent_revision = match topology_change
            .then(|| next_topology_revision(self.grid.id(), self.topology_revision))
            .transpose()
        {
            Ok(revision) => revision,
            Err(error) => return Err(SyncRejected::new(error, operation)),
        };
        let group = match routed_group(&mut self.members, target) {
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
            self.topology_revision = revision;
        }
        Ok(admission)
    }

    fn transact_local(
        &mut self,
        operation: SyncOperation<G>,
    ) -> Result<SyncAdmission, SyncRejected<G>> {
        let result = match &operation {
            SyncOperation::Topology { .. } => return self.transact_topology(operation),
            SyncOperation::Sync {
                intent,
                activation,
                transport,
                load,
                source,
                ..
            } => self.transact_intent(*intent, *load, *transport, *source, *activation),
            SyncOperation::Tempo {
                tempo,
                commit,
                smoothing,
                ..
            } => self.transact_tempo(*tempo, *commit, *smoothing),
            SyncOperation::Transport { .. }
            | SyncOperation::Prepare { .. }
            | SyncOperation::Relocate { .. } => {
                return self.transact_member(operation);
            }
        };
        result.map_err(|error| SyncRejected::new(error, operation))
    }

    /// Admits an operation addressed to one direct grid, which has no mode or
    /// timeline of its own.
    fn transact_member(
        &mut self,
        operation: SyncOperation<G>,
    ) -> Result<SyncAdmission, SyncRejected<G>> {
        let result = match &operation {
            SyncOperation::Transport {
                target,
                operation: TransportOperation::Seek { .. } | TransportOperation::PrepareStart { .. },
                ..
            } if self.applied_of(*target).is_some() => {
                Err(SyncError::RelocationRequired { member_id: *target })
            }
            SyncOperation::Transport {
                load, transport, ..
            } => {
                let (load, transport) = (*load, *transport);
                take_operation(self.grid.id(), &mut self.next_operation).map(|operation| {
                    self.blocked = None;
                    SyncAdmission::Accepted {
                        load,
                        transport,
                        operation,
                        topology: self.topology_stamp(),
                    }
                })
            }
            SyncOperation::Prepare {
                target,
                load,
                transport,
                source,
                window,
            } => self.transact_prepare(PrepareRequest {
                target: *target,
                load: *load,
                transport: *transport,
                source: *source,
                window: window.clone(),
            }),
            SyncOperation::Relocate {
                target,
                load,
                transport,
                cue,
                frontier,
                window,
            } => self.transact_relocate(RelocateRequest {
                target: *target,
                load: *load,
                transport: *transport,
                cue: *cue,
                frontier: *frontier,
                window: window.clone(),
            }),
            SyncOperation::Topology { .. }
            | SyncOperation::Sync { .. }
            | SyncOperation::Tempo { .. } => Err(SyncError::CapabilityUnavailable {
                capability: SyncCapability::Alignment,
            }),
        };
        result.map_err(|error| SyncRejected::new(error, operation))
    }

    fn transact_topology(
        &mut self,
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
        let reject = |error, operations| {
            SyncRejected::new(error, SyncOperation::Topology { base, operations })
        };
        let expected = self.topology_stamp();
        if base != expected {
            return Err(reject(
                SyncError::StaleTopology {
                    expected,
                    given: base,
                },
                operations,
            ));
        }

        let operation_id =
            match self
                .next_operation
                .ok_or_else(|| SyncError::OperationIdExhausted {
                    group_id: self.grid.id(),
                }) {
                Ok(operation_id) => operation_id,
                Err(error) => return Err(reject(error, operations)),
            };
        if operations.is_empty() {
            advance_operation(&mut self.next_operation);
            return Ok(SyncAdmission::Unchanged {
                operation: operation_id,
                topology: expected,
            });
        }

        let revision = match next_topology_revision(self.grid.id(), self.topology_revision) {
            Ok(revision) => revision,
            Err(error) => return Err(reject(error, operations)),
        };
        if let Err(error) = validate_topology_candidate(
            &self.grid,
            revision,
            &self.members,
            &operations,
            self.member_kind,
        ) {
            return Err(reject(error, operations));
        }
        let joins = match self.stage_joins(&operations) {
            Ok(joins) => joins,
            Err(error) => return Err(reject(error, operations)),
        };
        let restoration = match self.before_entry {
            Some((operation, prior))
                if self
                    .pending
                    .iter()
                    .any(|held| held.operation() == operation && !held.armed()) =>
            {
                match self.restored_entry_grid(prior) {
                    Ok(grid) => Some((prior, grid)),
                    Err(error) => return Err(reject(error, operations)),
                }
            }
            _ => None,
        };
        apply_topology_operations(&mut self.members, operations);
        let mut transition = self.retain_current_pending();
        if let Some((prior, grid)) = restoration {
            self.grid = grid;
            self.timeline = prior.timeline();
            self.before_entry = None;
            self.blocked = None;
        }
        for (id, staged) in joins {
            if let Some(SyncMember::Group { group, .. }) = self
                .members
                .iter_mut()
                .find(|member| BeatGridId::from(&**member) == id)
            {
                transition.append(group.apply_staged(staged));
            }
        }
        self.topology_revision = revision;
        advance_operation(&mut self.next_operation);
        Ok(SyncAdmission::TopologyChanged {
            operation: operation_id,
            topology: TopologyStamp::new(self.grid.id(), revision),
            transition,
        })
    }

    /// Stages every group a topology change brings in onto this group's
    /// session axis, before any of them joins.
    fn stage_joins(
        &self,
        operations: &[TopologyOperation<G>],
    ) -> Result<Vec<(BeatGridId, SyncStaged)>, SyncError> {
        let MapAxis::Session(axis) = self.grid.axis() else {
            return Ok(Vec::new());
        };
        operations
            .iter()
            .filter_map(|operation| match operation {
                TopologyOperation::Attach { member }
                | TopologyOperation::Replace {
                    replacement: member,
                    ..
                } => match member {
                    SyncMember::Group { group, .. } => Some(group),
                    SyncMember::Grid { .. } => None,
                },
                TopologyOperation::Detach { .. } => None,
            })
            .map(|group| {
                group
                    .stage_fact(ParentFact::Joined(SessionAxisUpdate::new(axis)))
                    .map(|staged| (group.id(), staged))
            })
            .collect()
    }
}

pub(super) fn take_operation(
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
