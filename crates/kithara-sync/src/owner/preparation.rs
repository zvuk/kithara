use std::ops::Range;

use kithara_signal::{SessionFrame, TransportRevision};
use kithara_warp::{
    BeatAlignment, BeatGridId, BeatGridQuery, BeatGridSnapshot, BeatGridState, MapRegion,
    WarpMapRevision, WarpPlan,
};

use super::{
    descent::{Parent, Staged, Takeover, output_transport},
    lifecycle::Applied,
    placement::{
        Missing, Placement, carry, continue_on, entry_window, place, place_mapped, project,
        retarget_boundary,
    },
    state::GroupState,
    timeline::Timeline,
    transaction::take_operation,
};
use crate::{
    AlignmentSource, LoadGeneration, SyncAdmission, SyncCapability, SyncEffect, SyncError,
    SyncExecutionStamp, SyncGroup, SyncMember, SyncOperationId, SyncPreparation, SyncTransition,
    TopologyStamp,
};

/// The one unapplied decision a group holds for a direct member.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pending {
    /// The member's preparation is issued to its executor.
    Prepared {
        preparation: SyncPreparation,
        entry: Entry,
        phase: Phase,
    },
    /// The member's preparation needs grid coverage not yet published.
    Waiting {
        member: BeatGridId,
        operation: SyncOperationId,
        load: LoadGeneration,
        transport: TransportRevision,
        required: MapRegion,
    },
}

/// How a preparation moves its member.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    /// A silent member starts to sound, and its activation must stay inside
    /// the launch window it was asked for.
    Launch(Range<SessionFrame>),
    /// A sounding member leaves its applied map.
    Replace,
    /// A public deck entry replans from its actual source inside this window.
    Public {
        source: AlignmentSource,
        window: Range<SessionFrame>,
    },
    /// A sounding member moves to an exact cue while its applied map keeps
    /// sounding; a new group grid withdraws it rather than carrying it.
    Relocate,
}

/// How far the executor carried a preparation out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    /// Issued and not acknowledged yet.
    Issued,
    /// Held by the executor, which may still drop it.
    Installed,
    /// Committed to the output; only its presentation or a new session axis
    /// ends it.
    Armed,
}

impl Pending {
    pub(super) fn member(&self) -> BeatGridId {
        match self {
            Self::Prepared { preparation, .. } => preparation.stamp().member().grid_id(),
            Self::Waiting { member, .. } => *member,
        }
    }

    pub(super) fn operation(&self) -> SyncOperationId {
        match self {
            Self::Prepared { preparation, .. } => preparation.stamp().operation(),
            Self::Waiting { operation, .. } => *operation,
        }
    }

    pub(super) fn load(&self) -> LoadGeneration {
        match self {
            Self::Prepared { preparation, .. } => preparation.stamp().load(),
            Self::Waiting { load, .. } => *load,
        }
    }

    pub(super) fn transport(&self) -> TransportRevision {
        match self {
            Self::Prepared { preparation, .. } => preparation.stamp().transport(),
            Self::Waiting { transport, .. } => *transport,
        }
    }

    pub(super) fn enters_map(&self) -> bool {
        match self {
            Self::Prepared { preparation, .. } => {
                matches!(preparation.effect(), SyncEffect::Projection { .. })
            }
            Self::Waiting { .. } => false,
        }
    }

    pub(super) const fn preparation(&self) -> Option<&SyncPreparation> {
        match self {
            Self::Prepared { preparation, .. } => Some(preparation),
            Self::Waiting { .. } => None,
        }
    }

    const fn relocates(&self) -> bool {
        matches!(
            self,
            Self::Prepared {
                entry: Entry::Relocate,
                ..
            }
        )
    }

    pub(super) const fn armed(&self) -> bool {
        matches!(
            self,
            Self::Prepared {
                phase: Phase::Armed,
                ..
            }
        )
    }
}

/// The request one preparation answers.
pub(super) struct PrepareRequest {
    pub(super) target: BeatGridId,
    pub(super) load: LoadGeneration,
    pub(super) transport: TransportRevision,
    pub(super) source: AlignmentSource,
    pub(super) window: Range<SessionFrame>,
}

/// The track entry carried by a deck's public Sync operation. It is planned
/// on the same staged successor as the mode change, before either is applied.
#[derive(Clone, Copy)]
pub(super) struct SyncEntry {
    pub(super) load: LoadGeneration,
    pub(super) transport: TransportRevision,
    pub(super) source: AlignmentSource,
    pub(super) activation: SessionFrame,
}

/// One admitted decision before it spends an operation identity.
pub(super) struct Decision {
    pub(super) member: BeatGridSnapshot,
    pub(super) load: LoadGeneration,
    pub(super) transport: TransportRevision,
    pub(super) entry: Entry,
    pub(super) replaces: Option<WarpMapRevision>,
}

/// Every direct member's decisions on a successor group grid, computed
/// before anything changes.
pub(super) struct Refreshed {
    pub(super) pending: Vec<Pending>,
    pub(super) applied: Vec<Applied>,
    pub(super) next_map: Option<WarpMapRevision>,
    pub(super) next_operation: Option<SyncOperationId>,
}

/// The facts one preparation is minted from, besides its placement.
struct Mint<'a> {
    owner: &'a BeatGridSnapshot,
    member: &'a BeatGridSnapshot,
    operation: SyncOperationId,
    topology: TopologyStamp,
    load: LoadGeneration,
    transport: TransportRevision,
    output_transport: Option<TransportRevision>,
    entry: Entry,
    replaces: Option<WarpMapRevision>,
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Adds the sole track owned by a leaf deck to a staged mode transition.
    /// Aggregate groups retain their existing timeline-only Sync semantics.
    pub(super) fn stage_sync_entry(
        &self,
        staged: &mut Staged,
        request: SyncEntry,
    ) -> Result<Option<SyncOperationId>, SyncError> {
        let [SyncMember::Grid { grid, .. }] = self.members.as_slice() else {
            return Ok(None);
        };
        let member = grid.snapshot();
        self.ensure_entry_identity(member.id(), request.load, request.transport)?;
        if let Some(held) = staged
            .pending
            .iter()
            .find(|held| held.member() == member.id() && held.armed())
        {
            return Err(SyncError::ArmedOperation {
                member_id: member.id(),
                operation: held.operation(),
            });
        }
        let previous = staged
            .applied
            .iter()
            .find(|lane| lane.member() == member.id());
        let replaces = previous.map(Applied::map);
        match request.source {
            AlignmentSource::Prepared(_) | AlignmentSource::Cued(_) if replaces.is_some() => {
                return Err(SyncError::MemberAudible {
                    member_id: member.id(),
                });
            }
            AlignmentSource::Audible { frontier, .. } if frontier.warp_map() != replaces => {
                return Err(SyncError::AudibleMapMismatch {
                    member_id: member.id(),
                    expected: replaces,
                    given: frontier.warp_map(),
                });
            }
            _ => {}
        }
        let window = entry_window(&staged.grid, &member, request.source, request.activation)
            .map_err(|missing| match missing {
                Missing::Coverage(_) => SyncError::GridCoverageUnavailable {
                    member_id: member.id(),
                },
                Missing::Refused(error) => error,
            })?;
        let entry = Entry::Public {
            source: request.source,
            window: window.clone(),
        };
        let placement = match previous {
            Some(lane) => place_mapped(&staged.grid, &member, request.source, &window, lane.plan()),
            None => place(&staged.grid, &member, request.source, &window),
        };
        let planned = placement.and_then(|placement| {
            project(
                &staged.grid,
                &member,
                placement,
                map_revision(&staged.grid, staged.next_map)?,
            )
        });
        let planned = match planned {
            Err(Missing::Coverage(_)) => {
                return Err(SyncError::GridCoverageUnavailable {
                    member_id: member.id(),
                });
            }
            Err(Missing::Refused(error)) => return Err(error),
            planned => planned,
        };
        let mut next_operation = staged.next_operation;
        let operation = take_operation(staged.grid.id(), &mut next_operation)?;
        let mut next_map = staged.next_map;
        let pending = Mint {
            owner: &staged.grid,
            member: &member,
            operation,
            topology: self.topology_stamp(),
            load: request.load,
            transport: request.transport,
            output_transport: staged.output_transport(),
            entry,
            replaces,
        }
        .pending(planned, &mut next_map)?;
        staged.next_operation = next_operation;
        staged.next_map = next_map;
        staged.pending.retain(|held| held.member() != member.id());
        staged.pending.push(pending);
        Ok(Some(operation))
    }

    /// Prepares one direct grid member to enter this group's beat timeline,
    /// or a sounding one to continue on it.
    ///
    /// A preparation replaces only the member's own pending decision and
    /// leaves its applied map in place. A refusal changes nothing and spends
    /// no operation identity; a missing coverage spends one, so the wait it
    /// reports stays addressable.
    pub(super) fn transact_prepare(
        &mut self,
        request: PrepareRequest,
    ) -> Result<SyncAdmission, SyncError> {
        let PrepareRequest {
            target,
            load,
            transport,
            source,
            window,
        } = request;
        let member = self.admissible_member(target)?;
        let lane = self.applied_of(target);
        let replaces = lane.map(Applied::map);
        let (entry, placement) = match (source, lane) {
            (AlignmentSource::Prepared(_) | AlignmentSource::Cued(_), Some(_)) => {
                return Err(SyncError::MemberAudible { member_id: target });
            }
            (AlignmentSource::Audible { frontier, .. }, _) if frontier.warp_map() != replaces => {
                return Err(SyncError::AudibleMapMismatch {
                    member_id: target,
                    expected: replaces,
                    given: frontier.warp_map(),
                });
            }
            (AlignmentSource::Audible { frontier, .. }, Some(lane)) => {
                let activation = window.start.max(frontier.output());
                if activation >= window.end {
                    return Err(SyncError::NoAdmissibleBoundary {
                        member_id: target,
                        first: activation,
                        end: window.end,
                    });
                }
                (
                    Entry::Replace,
                    continue_on(&self.grid, &member, lane.plan(), activation),
                )
            }
            (source, None) => (
                Entry::Launch(window.clone()),
                place(&self.grid, &member, source, &window),
            ),
        };
        self.admit(
            Decision {
                member,
                load,
                transport,
                entry,
                replaces,
            },
            placement,
        )
    }

    /// The frozen grid of the direct grid member `target`, if this group has
    /// a timeline to prepare it on and holds no armed preparation for it.
    pub(super) fn admissible_member(
        &self,
        target: BeatGridId,
    ) -> Result<BeatGridSnapshot, SyncError> {
        if matches!(self.timeline, Timeline::Off) {
            return Err(SyncError::CapabilityUnavailable {
                capability: SyncCapability::Alignment,
            });
        }
        let member = self
            .direct_grid(target)
            .ok_or_else(|| SyncError::MemberNotFound {
                group_id: self.grid.id(),
                member_id: target,
            })?;
        if let Some(held) = self.pending_of(target).filter(|held| held.armed()) {
            return Err(SyncError::ArmedOperation {
                member_id: target,
                operation: held.operation(),
            });
        }
        Ok(member)
    }

    /// Projects one placed decision and makes it the member's pending one,
    /// replacing what the member held.
    pub(super) fn admit(
        &mut self,
        decision: Decision,
        placement: Result<Placement, Missing>,
    ) -> Result<SyncAdmission, SyncError> {
        let Decision {
            member,
            load,
            transport,
            entry,
            replaces,
        } = decision;
        let target = member.id();
        self.ensure_entry_identity(target, load, transport)?;
        let mut next_map = self.next_map;
        let planned = placement.and_then(|placement| {
            project(
                &self.grid,
                &member,
                placement,
                map_revision(&self.grid, next_map)?,
            )
        });
        let planned = match planned {
            Err(Missing::Refused(error)) => return Err(error),
            planned => planned,
        };
        if matches!(&planned, Err(Missing::Coverage(_)))
            && self.before_entry.is_some_and(|(entry, _)| {
                self.pending
                    .iter()
                    .any(|held| held.operation() == entry && held.member() == target)
            })
        {
            return Err(SyncError::GridCoverageUnavailable { member_id: target });
        }
        let operation = take_operation(self.grid.id(), &mut self.next_operation)?;
        let topology = self.topology_stamp();
        let pending = Mint {
            owner: &self.grid,
            member: &member,
            operation,
            topology,
            load,
            transport,
            output_transport: output_transport(self.timeline, self.parent),
            entry,
            replaces,
        }
        .pending(planned, &mut next_map)?;
        let admission = match &pending {
            Pending::Prepared { preparation, .. } => SyncAdmission::Prepared(preparation.clone()),
            Pending::Waiting { required, .. } => SyncAdmission::Deferred {
                operation,
                topology,
                required: *required,
            },
        };
        self.next_map = next_map;
        self.before_entry = self.reconcile_before_entry(
            &self.grid,
            self.timeline,
            std::slice::from_ref(&pending),
            self.before_entry,
        );
        self.pending.retain(|held| held.member() != target);
        self.pending.push(pending);
        Ok(admission)
    }

    fn ensure_entry_identity(
        &self,
        member: BeatGridId,
        load: LoadGeneration,
        transport: TransportRevision,
    ) -> Result<(), SyncError> {
        let Some((operation, _)) = self.before_entry else {
            return Ok(());
        };
        let Some(previous) = self
            .pending
            .iter()
            .find(|held| held.operation() == operation && held.member() == member)
            .and_then(Pending::preparation)
        else {
            return Ok(());
        };
        let stamp = previous.stamp();
        if stamp.load() != load || stamp.transport() != transport {
            return Err(SyncError::EntryIdentityMismatch {
                member_id: member,
                expected_load: stamp.load(),
                given_load: load,
                expected_transport: stamp.transport(),
                given_transport: transport,
            });
        }
        Ok(())
    }
}

impl<G: SyncGroup<NestedGroup = G>> GroupState<G> {
    /// Carries every direct member's decisions onto the successor `grid`
    /// under `timeline`, without changing anything.
    ///
    /// An armed preparation is kept as it is. A launch keeps its operation
    /// and the beats that sound together and gets a new map on the new grid;
    /// one whose activation leaves its window, or whose member grid changed
    /// since, is withdrawn. A sounding member is retargeted at the selected
    /// activation, continuing the recording its applied map plays there; a
    /// relocation it held is withdrawn, and the retarget is a new operation.
    /// A Host-to-Local handoff uses the exact latch cut instead of choosing a
    /// later beat, so the Local timeline and replacement map start together. A
    /// timeline without geometry withdraws every decision but a handoff, and
    /// a new axis withdraws everything, applied maps too.
    pub(super) fn refreshed(
        &self,
        grid: &BeatGridSnapshot,
        timeline: Timeline,
        parent: Option<Parent>,
        takeover: Takeover,
    ) -> Result<Refreshed, SyncError> {
        let Takeover {
            commit,
            mut next_operation,
        } = takeover;
        if let Some(refreshed) = self.refreshed_without_replan(grid, next_operation) {
            return Ok(refreshed);
        }
        let mut next_map = self.next_map;
        let output_transport = output_transport(timeline, parent);
        let execution_floor = parent
            .and_then(Parent::segment)
            .and_then(|segment| segment.execution_floor());
        let live = !matches!(timeline, Timeline::Off) && grid.state() == BeatGridState::Live;
        let mut pending: Vec<Pending> = Vec::with_capacity(self.pending.len());
        for member in self.members.iter().filter_map(|member| match member {
            SyncMember::Grid { grid, .. } => Some(grid.snapshot()),
            SyncMember::Group { .. } => None,
        }) {
            let held = self.pending_of(member.id());
            let decided = match (held, self.applied_of(member.id())) {
                (Some(held), _) if held.armed() => Some(held.clone()),
                (Some(held), _) if !live => handoff(held).cloned(),
                (_, Some(lane)) if live => {
                    let replanned = public_on_host(
                        held,
                        timeline,
                        grid,
                        &member,
                        Some(lane),
                        output_transport,
                        &mut next_map,
                    )?;
                    let public_missed = matches!(replanned, Some(None));
                    let first_entry = public_missed
                        && self.before_entry.is_some_and(|(operation, _)| {
                            held.is_some_and(|pending| pending.operation() == operation)
                        });
                    if let Some(Some(candidate)) = replanned {
                        Some(candidate)
                    } else if first_entry {
                        None
                    } else {
                        let Some(commit) = commit else {
                            continue;
                        };
                        let returns_from_unclaimed_entry = !matches!(timeline, Timeline::Host)
                            && self.before_entry.is_some_and(|(entry, _)| {
                                held.is_some_and(|pending| pending.operation() == entry)
                            });
                        let leaves_host_for_local = matches!(self.timeline, Timeline::Host)
                            && matches!(timeline, Timeline::Local(_));
                        let stamp = lane.applied().stamp();
                        let (operation, load, transport) = match held {
                            Some(held)
                                if !held.relocates()
                                    && !returns_from_unclaimed_entry
                                    && !public_missed =>
                            {
                                (held.operation(), held.load(), held.transport())
                            }
                            Some(_) | None => (
                                take_operation(self.grid.id(), &mut next_operation)?,
                                stamp.load(),
                                stamp.transport(),
                            ),
                        };
                        let activation = if leaves_host_for_local {
                            Ok(commit)
                        } else {
                            retarget_boundary(
                                grid,
                                commit.max(execution_floor.unwrap_or(commit)),
                                lane.applied().frontier().output(),
                            )
                        };
                        let planned = activation
                            .and_then(|activation| {
                                continue_on(grid, &member, lane.plan(), activation)
                            })
                            .and_then(|placement| {
                                project(grid, &member, placement, map_revision(grid, next_map)?)
                            });
                        let mint = Mint {
                            owner: grid,
                            member: &member,
                            operation,
                            topology: self.topology_stamp(),
                            load,
                            transport,
                            output_transport,
                            entry: Entry::Replace,
                            replaces: Some(lane.map()),
                        };
                        Some(mint.pending(planned, &mut next_map)?)
                    }
                }
                (Some(waiting @ Pending::Waiting { .. }), None) => Some(waiting.clone()),
                (
                    Some(
                        held @ Pending::Prepared {
                            entry: Entry::Public { .. },
                            ..
                        },
                    ),
                    None,
                ) if live && matches!(timeline, Timeline::Host) => public_on_host(
                    Some(held),
                    timeline,
                    grid,
                    &member,
                    None,
                    output_transport,
                    &mut next_map,
                )?
                .flatten(),
                (
                    Some(Pending::Prepared {
                        preparation,
                        entry: Entry::Launch(window),
                        ..
                    }),
                    None,
                ) if preparation.stamp().member() == member.stamp() => Self::carry_launch(
                    grid,
                    &member,
                    preparation,
                    window,
                    output_transport,
                    &mut next_map,
                )?,
                _ => None,
            };
            pending.extend(decided);
        }
        Ok(Refreshed {
            pending,
            applied: self.applied.clone(),
            next_map,
            next_operation,
        })
    }

    fn refreshed_without_replan(
        &self,
        grid: &BeatGridSnapshot,
        next_operation: Option<SyncOperationId>,
    ) -> Option<Refreshed> {
        if grid.stamp() == self.grid.stamp() {
            Some(Refreshed {
                pending: self.pending.clone(),
                applied: self.applied.clone(),
                next_map: self.next_map,
                next_operation,
            })
        } else if grid.axis() != self.grid.axis() {
            Some(Refreshed {
                pending: Vec::new(),
                applied: Vec::new(),
                next_map: self.next_map,
                next_operation,
            })
        } else {
            None
        }
    }

    fn carry_launch(
        owner: &BeatGridSnapshot,
        member: &BeatGridSnapshot,
        preparation: &SyncPreparation,
        window: &Range<SessionFrame>,
        output_transport: Option<TransportRevision>,
        next_map: &mut Option<WarpMapRevision>,
    ) -> Result<Option<Pending>, SyncError> {
        let SyncEffect::Projection { alignment, .. } = preparation.effect() else {
            return Ok(None);
        };
        let planned = match carry(owner, member, *alignment, window) {
            Ok(Some(placement)) => map_revision(owner, *next_map)
                .and_then(|revision| project(owner, member, placement, revision)),
            Ok(None) => return Ok(None),
            Err(missing) => Err(missing),
        };
        let stamp = preparation.stamp();
        let mint = Mint {
            owner,
            member,
            operation: stamp.operation(),
            topology: stamp.topology(),
            load: stamp.load(),
            transport: stamp.transport(),
            output_transport,
            entry: Entry::Launch(window.clone()),
            replaces: None,
        };
        Ok(Some(mint.pending(planned, next_map)?))
    }

    /// Releases every sounding member of a group that leaves its timeline at
    /// `activation`: each one continues unsynchronized from the recording
    /// frame its applied map reaches there.
    pub(super) fn handoffs(
        &self,
        group: &BeatGridSnapshot,
        operation: SyncOperationId,
        transport: TransportRevision,
        activation: SessionFrame,
    ) -> Result<Vec<Pending>, SyncError> {
        if let Some(held) = self.pending.iter().find(|held| held.armed()) {
            return Err(SyncError::ArmedOperation {
                member_id: held.member(),
                operation: held.operation(),
            });
        }
        let mut handoffs: Vec<Pending> = Vec::with_capacity(self.applied.len());
        for lane in &self.applied {
            let member =
                self.direct_grid(lane.member())
                    .ok_or_else(|| SyncError::MemberNotFound {
                        group_id: self.grid.id(),
                        member_id: lane.member(),
                    })?;
            let BeatGridQuery::Resolved(source) = lane.plan().source_at(activation) else {
                return Err(SyncError::OutsideGrid {
                    grid_id: member.id(),
                });
            };
            let preparation = SyncPreparation::new(
                SyncExecutionStamp::new(
                    operation,
                    member.stamp(),
                    group.stamp(),
                    self.topology_stamp(),
                    lane.applied().stamp().load(),
                    transport,
                ),
                SyncEffect::Handoff {
                    replaces: lane.map(),
                    source,
                    activation,
                },
            );
            handoffs.push(Pending::Prepared {
                preparation,
                entry: Entry::Replace,
                phase: Phase::Issued,
            });
        }
        Ok(handoffs)
    }

    /// Drops, on a topology change, every decision whose member left the
    /// group and every issued or installed preparation: each was stamped with
    /// the topology being replaced, and an execution receipt is never
    /// restamped past a new fence. What is armed or applied already sounds,
    /// so a member that stays keeps it.
    pub(super) fn retain_current_pending(&mut self) -> SyncTransition {
        let held = std::mem::take(&mut self.pending);
        self.pending = held
            .iter()
            .filter(|held| {
                self.direct_grid(held.member()).is_some()
                    && !matches!(
                        held,
                        Pending::Prepared {
                            phase: Phase::Issued | Phase::Installed,
                            ..
                        }
                    )
            })
            .cloned()
            .collect();
        let applied = std::mem::take(&mut self.applied);
        self.applied = applied
            .into_iter()
            .filter(|lane| self.direct_grid(lane.member()).is_some())
            .collect();
        transition(&held, &self.pending)
    }

    /// Returns the frozen grid of the direct grid member `id`.
    pub(super) fn direct_grid(&self, id: BeatGridId) -> Option<BeatGridSnapshot> {
        self.members.iter().find_map(|member| match member {
            SyncMember::Grid { grid, .. } if grid.id() == id => Some(grid.snapshot()),
            SyncMember::Grid { .. } | SyncMember::Group { .. } => None,
        })
    }

    fn pending_of(&self, member: BeatGridId) -> Option<&Pending> {
        self.pending.iter().find(|held| held.member() == member)
    }
}

/// Distinguishes a public Host entry from an ordinary applied-lane retarget.
fn public_on_host(
    held: Option<&Pending>,
    timeline: Timeline,
    owner: &BeatGridSnapshot,
    member: &BeatGridSnapshot,
    lane: Option<&Applied>,
    output_transport: Option<TransportRevision>,
    next_map: &mut Option<WarpMapRevision>,
) -> Result<Option<Option<Pending>>, SyncError> {
    let Some(Pending::Prepared {
        preparation,
        entry: Entry::Public { source, window },
        ..
    }) = held
    else {
        return Ok(None);
    };
    if !matches!(timeline, Timeline::Host) {
        return Ok(None);
    }
    let placement = lane.map_or_else(
        || place(owner, member, *source, window),
        |lane| place_mapped(owner, member, *source, window, lane.plan()),
    );
    let planned = placement
        .and_then(|placement| project(owner, member, placement, map_revision(owner, *next_map)?));
    let planned = match planned {
        Ok(planned) => planned,
        Err(Missing::Refused(SyncError::NoAdmissibleBoundary { .. })) => return Ok(Some(None)),
        Err(Missing::Coverage(_)) => {
            return Err(SyncError::GridCoverageUnavailable {
                member_id: member.id(),
            });
        }
        Err(Missing::Refused(error)) => return Err(error),
    };
    let stamp = preparation.stamp();
    let pending = Mint {
        owner,
        member,
        operation: stamp.operation(),
        topology: stamp.topology(),
        load: stamp.load(),
        transport: stamp.transport(),
        output_transport,
        entry: Entry::Public {
            source: *source,
            window: window.clone(),
        },
        replaces: lane.map(Applied::map),
    }
    .pending(Ok(planned), next_map)?;
    Ok(Some(Some(pending)))
}

/// The preparations `next` issues and withdraws compared with `held`: each
/// one a member did not hold before is issued, and each one whose operation
/// no preparation of its member carries on afterwards is withdrawn.
pub(super) fn transition(held: &[Pending], next: &[Pending]) -> SyncTransition {
    let issued = next
        .iter()
        .filter_map(Pending::preparation)
        .filter(|preparation| {
            !held
                .iter()
                .any(|old| old.preparation() == Some(preparation))
        })
        .cloned()
        .collect();
    let withdrawn = held
        .iter()
        .filter_map(Pending::preparation)
        .filter(|preparation| {
            let member = preparation.stamp().member().grid_id();
            let operation = preparation.stamp().operation();
            !next.iter().any(|new| {
                new.preparation().is_some()
                    && new.member() == member
                    && new.operation() == operation
            })
        })
        .map(SyncPreparation::stamp)
        .collect();
    SyncTransition::new(issued, withdrawn)
}

/// `held` when it still releases its member from a timeline that lost its
/// geometry: a handoff.
fn handoff(held: &Pending) -> Option<&Pending> {
    match held {
        Pending::Prepared { preparation, .. }
            if matches!(preparation.effect(), SyncEffect::Handoff { .. }) =>
        {
            Some(held)
        }
        Pending::Prepared { .. } | Pending::Waiting { .. } => None,
    }
}

impl Mint<'_> {
    /// Turns one planning result into the member's pending decision, spending
    /// the map revision a projection takes.
    fn pending(
        self,
        planned: Result<(BeatAlignment, WarpPlan), Missing>,
        next_map: &mut Option<WarpMapRevision>,
    ) -> Result<Pending, SyncError> {
        match planned {
            Ok((alignment, plan)) => {
                *next_map = plan.activation().revision().checked_next();
                Ok(Pending::Prepared {
                    preparation: SyncPreparation::new(
                        SyncExecutionStamp::new(
                            self.operation,
                            self.member.stamp(),
                            self.owner.stamp(),
                            self.topology,
                            self.load,
                            self.transport,
                        )
                        .with_output_transport(self.output_transport),
                        SyncEffect::Projection {
                            alignment,
                            plan,
                            replaces: self.replaces,
                        },
                    ),
                    entry: self.entry,
                    phase: Phase::Issued,
                })
            }
            Err(Missing::Coverage(required)) => Ok(Pending::Waiting {
                member: self.member.id(),
                operation: self.operation,
                load: self.load,
                transport: self.transport,
                required,
            }),
            Err(Missing::Refused(error)) => Err(error),
        }
    }
}

/// The map revision the next projection on `owner` takes.
fn map_revision(
    owner: &BeatGridSnapshot,
    next_map: Option<WarpMapRevision>,
) -> Result<WarpMapRevision, Missing> {
    next_map.ok_or_else(|| {
        Missing::Refused(SyncError::WarpMapRevisionExhausted {
            group_id: owner.id(),
        })
    })
}
