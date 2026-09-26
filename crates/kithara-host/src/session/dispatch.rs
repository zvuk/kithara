use std::num::NonZeroU32;

use firewheel::{FirewheelContext, error::UpdateError};
use kithara_bufpool::HasPool;
use kithara_output::OutputGroup;
use kithara_platform::sync::Arc;
#[cfg(any(target_arch = "wasm32", test))]
use kithara_platform::sync::mpsc;
use kithara_play::{
    PlayError, StreamShape,
    player::{ResidentRender, ResidentStaging},
};
use kithara_signal::SessionFrame;
use kithara_sync::{
    AlignmentSource, ArmPermit, ControlEnterError, ControlGuard, GroupState, PermitCell,
    PreparedRevocation, SyncAdmission, SyncCapability, SyncError, SyncExecutionReject, SyncGroup,
    SyncIntent, SyncMode, SyncOperation, SyncReceipt, SyncReceiptAck, SyncRejected, SyncTransition,
    TopologyOperation,
};
use kithara_warp::{BeatGrid, BeatGridId};
use tracing::{debug, trace, warn};

#[cfg(any(target_arch = "wasm32", test))]
use super::protocol::HostCmdMsg;
use super::{
    graph::{controls, lifecycle, player_index, slots, tap},
    protocol::{
        Cmd, HostCmd, HostReply, PlayerId, PlayerLevel, Reply, SessionError, SessionSampleRate,
        SyncCmd,
    },
    state::{SessionState, register_player},
    transport,
    transport::RouteRestartStatus,
};
use crate::{
    PlayerMember,
    api::{DeckSyncState, HostLevel},
};

pub(crate) fn run_host_cmd<T, S>(state: &mut SessionState<T, S>, cmd: HostCmd<S>) -> HostReply
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    match cmd {
        HostCmd::Play(Cmd::AcknowledgeSync { receipt }) => {
            let arbiter = state.sync_arbiter.clone();
            let control = match arbiter.enter_host_control() {
                Ok(control) => control,
                Err(error) => {
                    if error == ControlEnterError::Busy
                        && let Err(queue_error) = queue_failed_gate_receipt(state, receipt)
                    {
                        return HostReply::Play(Reply::Err(queue_error));
                    }
                    return control_failure_reply(
                        HostCmd::<S>::Play(Cmd::AcknowledgeSync { receipt }),
                        error,
                    );
                }
            };
            if let Err(error) = drain_audio_receipts(state, &control) {
                return HostReply::Play(Reply::Err(error));
            }
            if let Err(error) = drain_failed_gate_receipts(state) {
                return HostReply::Play(Reply::Err(error));
            }
            if let Err(error) = transport::observe_commits(state, &control) {
                return HostReply::Play(Reply::Err(error.into()));
            }
            HostReply::Play(match acknowledge_root(state, receipt, &control) {
                Ok(answer) => Reply::SyncAcknowledged(answer),
                Err(error) => Reply::Err(error),
            })
        }
        HostCmd::Play(Cmd::RetireSyncMember { member }) => {
            let arbiter = state.sync_arbiter.clone();
            let control = match arbiter.enter_host_control() {
                Ok(control) => control,
                Err(error) => {
                    return control_failure_reply(
                        HostCmd::<S>::Play(Cmd::RetireSyncMember { member }),
                        error,
                    );
                }
            };
            if let Err(error) = drain_audio_receipts(state, &control) {
                return HostReply::Play(Reply::Err(error));
            }
            if let Err(error) = drain_failed_gate_receipts(state) {
                return HostReply::Play(Reply::Err(error));
            }
            HostReply::Play(
                state
                    .retire_sync_member(member, &control)
                    .map_or_else(Reply::Err, |()| Reply::Ok),
            )
        }
        HostCmd::Play(cmd) => match pump_before_work(state) {
            Ok(()) => HostReply::Play(run_cmd(state, cmd)),
            Err(error) => HostReply::Play(Reply::Err(error)),
        },
        HostCmd::Sync(cmd) => {
            let arbiter = state.sync_arbiter.clone();
            let _control = match arbiter.enter_host_control() {
                Ok(control) => control,
                Err(error) => return control_failure_reply(HostCmd::<S>::Sync(cmd), error),
            };
            if let Err(error) = drain_audio_receipts(state, &_control) {
                return HostReply::Err(error.into());
            }
            if let Err(error) = drain_failed_gate_receipts(state) {
                return HostReply::Err(error.into());
            }
            run_sync_cmd(state, cmd, &_control)
        }
        HostCmd::ApplyMix { levels } => {
            if let Err(error) = pump_before_work(state) {
                return HostReply::Err(error.into());
            }
            apply_mix(state, &levels).map_or_else(HostReply::Err, |()| HostReply::Ok)
        }
        HostCmd::EnableOutput { outputs } => {
            if let Err(error) = pump_before_work(state) {
                return HostReply::Err(error.into());
            }
            tap::enable(state, outputs)
                .map_or_else(|error| HostReply::Err(error.into()), |()| HostReply::Ok)
        }
        HostCmd::Shutdown => HostReply::Ok,
    }
}

fn enter_error(error: ControlEnterError) -> SessionError {
    match error {
        ControlEnterError::Busy => SessionError::SyncControlBusy,
        ControlEnterError::Closed => SessionError::Sync(SyncError::OwnerUnavailable),
    }
}

/// Pump the Host's RT mailboxes under one short owner cut before any backend
/// or stream work. The backend operation itself runs after this guard drops.
pub(super) fn pump_before_work<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    let arbiter = state.sync_arbiter.clone();
    let control = arbiter.enter_host_control().map_err(enter_error)?;
    drain_audio_receipts(state, &control)?;
    drain_failed_gate_receipts(state)?;
    transport::observe_commits(state, &control)?;
    Ok(())
}

/// Enter only for a canonical owner publication. The caller's Firewheel or
/// stream work must happen before or after this short cut.
pub(super) fn with_owner_cut<T, S, R>(
    state: &mut SessionState<T, S>,
    publish: impl FnOnce(&mut SessionState<T, S>, &ControlGuard<'_>) -> Result<R, SessionError>,
) -> Result<R, SessionError> {
    let arbiter = state.sync_arbiter.clone();
    let control = arbiter.enter_host_control().map_err(enter_error)?;
    drain_audio_receipts(state, &control)?;
    drain_failed_gate_receipts(state)?;
    publish(state, &control)
}

/// Commit one root grid publication while RT claims are excluded. Preflight
/// every cell before the owner changes, then revoke only transitions that
/// actually replace a ticket, except a new axis/epoch which fences all cells.
pub(super) fn publish_root_transition<T, S>(
    state: &mut SessionState<T, S>,
    control: &ControlGuard<'_>,
    axis_changed: bool,
    publish: impl FnOnce(&mut GroupState<PlayerMember>) -> Result<SyncTransition, SyncError>,
) -> Result<(), SyncError> {
    let cells: Vec<_> = state
        .sync_cells
        .iter()
        .filter(|entry| {
            axis_changed
                || state.root.with_group(entry.group, SyncGroup::mode) == Some(SyncMode::HostSync)
        })
        .map(|entry| Arc::clone(&entry.cell))
        .collect();
    let prepared = cells
        .iter()
        .map(|cell| control.preflight_revoke(cell))
        .collect::<Result<Vec<_>, _>>()
        .map_err(SyncError::ExecutionControl)?;
    let transition = publish(&mut state.root)?;
    for (cell, revoke) in cells.iter().zip(prepared) {
        if axis_changed || transition_replaces_ticket(&transition, cell.member()) {
            revoke.revoke();
        }
    }
    state.publish_root();
    Ok(())
}

/// Final owner drain after graph retirement and before a slot receiver is
/// destroyed. A deck whose last processor is gone also withdraws only that
/// track's remaining decisions. Backend teardown has already completed
/// outside this short cut.
pub(super) fn drain_after_quiescence<T, S>(
    state: &mut SessionState<T, S>,
    quiesced_deck: Option<BeatGridId>,
) -> Result<(), SessionError> {
    let arbiter = state.sync_arbiter.clone();
    let control = arbiter.enter_host_control().map_err(enter_error)?;
    drain_audio_receipts(state, &control)?;
    drain_failed_gate_receipts(state)?;
    let Some(group) = quiesced_deck else {
        return Ok(());
    };
    let Some(cell) = state
        .sync_cells
        .iter()
        .find(|entry| entry.group == group)
        .map(|entry| Arc::clone(&entry.cell))
    else {
        return Ok(());
    };
    let revocation = control
        .preflight_revoke(&cell)
        .map_err(SyncError::ExecutionControl)?;
    let _admission = state
        .root
        .transact(SyncOperation::WithdrawQuiescedMember {
            target: cell.member(),
        })
        .map_err(|rejected| SessionError::Sync(rejected.error().clone()))?;
    revocation.revoke();
    state.publish_root();
    Ok(())
}

/// A timed-out Installed never reached the owner. The Host retains one
/// terminal rejection for that member, then applies it on the next Control
/// cut after the in-flight audio claim completes. The executor drops its lane
/// immediately and cannot accidentally arm it.
fn queue_failed_gate_receipt<T, S>(
    state: &mut SessionState<T, S>,
    receipt: SyncReceipt,
) -> Result<(), SessionError> {
    let terminal = match receipt {
        SyncReceipt::Installed(stamp) => SyncReceipt::Rejected {
            stamp,
            reason: SyncExecutionReject::ControlBusy,
        },
        rejected @ SyncReceipt::Rejected { .. } => rejected,
        _ => return Err(SessionError::Graph("executor sent an audio receipt".into())),
    };
    let member = match terminal {
        SyncReceipt::Rejected { stamp, .. } => stamp.member().grid_id(),
        _ => {
            return Err(SessionError::Graph(
                "missing terminal rejection stamp".into(),
            ));
        }
    };
    let entry = state
        .sync_cells
        .iter_mut()
        .find(|entry| entry.cell.member() == member)
        .ok_or(SessionError::SyncMemberNotRegistered(member))?;
    match entry.pending_gate_receipt {
        Some(existing) if existing == terminal => Ok(()),
        Some(_) => Err(SessionError::Graph(
            "member already has an undrained gate failure".into(),
        )),
        None => {
            entry.pending_gate_receipt = Some(terminal);
            Ok(())
        }
    }
}

fn drain_failed_gate_receipts<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    for index in 0..state.sync_cells.len() {
        let Some(receipt) = state.sync_cells[index].pending_gate_receipt else {
            continue;
        };
        if let Err(error) = state.root.acknowledge(receipt)
            && !error.is_superseded_rejection(receipt)
        {
            return Err(error.into());
        }
        state.sync_cells[index].pending_gate_receipt = None;
        state.publish_root();
    }
    Ok(())
}

/// Sole consumer of the per-slot callback receipts. A failed owner update
/// keeps the popped receipt in its fixed slot for a later owner cut.
fn drain_audio_receipts<T, S>(
    state: &mut SessionState<T, S>,
    control: &ControlGuard<'_>,
) -> Result<(), SessionError> {
    for deck_index in 0..state.graph.len() {
        let slots = state
            .graph
            .deck(deck_index)
            .map_or(0, |deck| deck.slots.len());
        for slot_index in 0..slots {
            loop {
                let receipt = state
                    .graph
                    .deck_mut(deck_index)
                    .and_then(|deck| deck.slots.get_mut(slot_index))
                    .and_then(|slot| {
                        slot.pending_receipt
                            .take()
                            .or_else(|| slot.sync_receipts.try_pop())
                    });
                let Some(receipt) = receipt else { break };
                let result = match receipt {
                    SyncReceipt::Armed(_)
                    | SyncReceipt::Presented(_)
                    | SyncReceipt::Rejected { .. } => {
                        acknowledge_root(state, receipt, control).map(|_| ())
                    }
                    _ => Err(SessionError::Graph(
                        "RT mailbox carried a non-audio sync receipt".into(),
                    )),
                };
                if let Err(error) = result {
                    if let Some(slot) = state
                        .graph
                        .deck_mut(deck_index)
                        .and_then(|deck| deck.slots.get_mut(slot_index))
                    {
                        slot.pending_receipt = Some(receipt);
                    }
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn control_failure_reply<S>(cmd: HostCmd<S>, failure: ControlEnterError) -> HostReply {
    let sync_error = match failure {
        ControlEnterError::Busy => SyncError::ControlBusy,
        ControlEnterError::Closed => SyncError::OwnerUnavailable,
    };
    let session_error = match failure {
        ControlEnterError::Busy => SessionError::SyncControlBusy,
        ControlEnterError::Closed => SessionError::Sync(SyncError::OwnerUnavailable),
    };
    match cmd {
        HostCmd::Sync(SyncCmd::Transact(operation)) => {
            HostReply::Admission(Err(SyncRejected::new(sync_error, operation)))
        }
        HostCmd::Play(_) => HostReply::Play(Reply::Err(session_error)),
        HostCmd::Shutdown => HostReply::Ok,
        HostCmd::Sync(SyncCmd::TransactCurrent(_))
        | HostCmd::Sync(SyncCmd::QueryDeckState { .. })
        | HostCmd::Sync(SyncCmd::RequestDeckSync { .. })
        | HostCmd::ApplyMix { .. }
        | HostCmd::EnableOutput { .. } => HostReply::Err(session_error.into()),
    }
}

fn run_sync_cmd<T, S>(
    state: &mut SessionState<T, S>,
    cmd: SyncCmd,
    control: &ControlGuard<'_>,
) -> HostReply {
    let operation = match cmd {
        SyncCmd::RequestDeckSync {
            target,
            member,
            intent,
            observation,
        } => {
            if let Err(error) = transport::observe_commits(state, control) {
                return HostReply::Err(SessionError::from(error).into());
            }
            let Some(resident) = observation else {
                return HostReply::Err(PlayError::NotReady);
            };
            if matches!(intent, SyncIntent::Enable | SyncIntent::AlignNow)
                && resident.staging() != ResidentStaging::Available
            {
                return HostReply::Err(PlayError::NotReady);
            }
            let ResidentRender::Snapshot(snapshot) = resident.render() else {
                return HostReply::Err(PlayError::NotReady);
            };
            let Some(processed) = state
                .transport_control
                .as_mut()
                .and_then(|transport| transport.observation().snapshot())
            else {
                return HostReply::Err(SessionError::TransportNotProcessed.into());
            };
            let output = snapshot.context().output();
            if output.session_epoch() != processed.session_epoch()
                || output.transport_revision() != Some(processed.revision())
                || output.sample_rate() != processed.session_grid().axis().sample_rate()
            {
                return HostReply::Err(PlayError::NotReady);
            }
            if !state
                .sync_cells
                .iter()
                .any(|entry| entry.cell.member() == member)
            {
                return HostReply::Err(SessionError::SyncMemberNotRegistered(member).into());
            }
            let (boundary, _) = match transport::commit_boundary(state) {
                Ok(boundary) => boundary,
                Err(error) => return HostReply::Err(error.into()),
            };
            let lower_bound = boundary.max(output.output_frames().end);
            let Some(activation) = i64::from(lower_bound).checked_add(2048) else {
                return HostReply::Err(SessionError::TransportFrameExhausted.into());
            };
            SyncOperation::Sync {
                target,
                load: resident.load(),
                transport: processed.revision(),
                source: AlignmentSource::Audible {
                    frontier: snapshot.frontier(),
                    speed: resident.requested_speed(),
                },
                activation: SessionFrame::new(activation),
                intent,
            }
        }
        SyncCmd::QueryDeckState { target } => {
            return state
                .root
                .with_group(target, |group| DeckSyncState {
                    mode: group.mode(),
                    status: group.status(),
                })
                .map_or_else(
                    || {
                        HostReply::Err(
                            SessionError::from(SyncError::GroupNotFound { group_id: target })
                                .into(),
                        )
                    },
                    HostReply::DeckSyncState,
                );
        }
        SyncCmd::Transact(operation) => match transport::observe_commits(state, control) {
            Ok(()) => operation,
            Err(error) => return HostReply::Admission(Err(SyncRejected::new(error, operation))),
        },
        SyncCmd::TransactCurrent(operations) => {
            let topology = match transport::observe_commits(state, control)
                .and_then(|()| state.root.topology())
            {
                Ok(topology) => topology,
                Err(error) => return HostReply::Err(SessionError::from(error).into()),
            };
            SyncOperation::Topology {
                operations,
                base: topology.stamp(),
            }
        }
    };
    let result = transact_root(state, operation, control);
    if result.is_ok() {
        state.publish_root();
    }
    HostReply::Admission(result)
}

/// Records one executor receipt on the root group and publishes the state it
/// leaves; a refused receipt changes nothing and publishes nothing.
fn acknowledge_root<T, S>(
    state: &mut SessionState<T, S>,
    receipt: SyncReceipt,
    control: &ControlGuard<'_>,
) -> Result<SyncReceiptAck, SessionError> {
    let permit: Option<ArmPermit> = match receipt {
        SyncReceipt::Installed(stamp) => {
            let member = stamp.member().grid_id();
            let cell = state
                .sync_cells
                .iter()
                .find(|entry| entry.cell.member() == member)
                .ok_or(SessionError::SyncMemberNotRegistered(member))?;
            Some(control.mint_permit(&cell.cell, stamp)?)
        }
        _ => None,
    };
    if let Err(error) = state.root.acknowledge(receipt)
        && !error.is_superseded_rejection(receipt)
    {
        return Err(error.into());
    }
    state.publish_root();
    Ok(permit.map_or(SyncReceiptAck::Recorded, SyncReceiptAck::Installed))
}

fn transact_root<T, S>(
    state: &mut SessionState<T, S>,
    operation: SyncOperation<PlayerMember>,
    control: &ControlGuard<'_>,
) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
    if let SyncOperation::WithdrawQuiescedMember { target } = &operation {
        return Err(SyncRejected::new(
            SyncError::QuiescenceRequired { member_id: *target },
            operation,
        ));
    }
    if topology_conflicts_with_graph(state, &operation) {
        return Err(SyncRejected::new(
            SyncError::CapabilityUnavailable {
                capability: SyncCapability::Topology,
            },
            operation,
        ));
    }
    let affected = affected_cells(state, &operation);
    let mut prepared: Vec<PreparedRevocation<'_, '_, '_>> = Vec::with_capacity(affected.len());
    for cell in &affected {
        let revoke = match control.preflight_revoke(cell) {
            Ok(revoke) => revoke,
            Err(error) => {
                return Err(SyncRejected::new(
                    SyncError::ExecutionControl(error),
                    operation,
                ));
            }
        };
        prepared.push(revoke);
    }
    let admission = state.root.transact(operation)?;
    for (cell, revoke) in affected.iter().zip(prepared) {
        if admission_replaces_ticket(&admission, cell.member()) {
            revoke.revoke();
        }
    }
    Ok(admission)
}

/// Only cells belonging to the transaction's affected deck or subtree are
/// preflighted. An unrelated deck's reserved source never blocks this edit.
fn affected_cells<T, S>(
    state: &SessionState<T, S>,
    operation: &SyncOperation<PlayerMember>,
) -> Vec<Arc<PermitCell>> {
    let mut targets: Vec<BeatGridId> = Vec::new();
    match operation {
        SyncOperation::Topology { operations, .. } => {
            for edit in operations {
                match edit {
                    TopologyOperation::Attach { member } => targets.push(member.id()),
                    TopologyOperation::Detach { member } => targets.push(*member),
                    TopologyOperation::Replace {
                        member,
                        replacement,
                    } => {
                        targets.push(*member);
                        targets.push(replacement.id());
                    }
                }
            }
        }
        SyncOperation::Tempo { target, .. } if *target == state.root.id() => {
            return state
                .sync_cells
                .iter()
                .filter(|entry| {
                    state.root.with_group(entry.group, SyncGroup::mode) == Some(SyncMode::HostSync)
                })
                .map(|entry| Arc::clone(&entry.cell))
                .collect();
        }
        _ => targets.push(operation.target()),
    }
    state
        .sync_cells
        .iter()
        .filter(|entry| {
            targets
                .iter()
                .any(|target| *target == entry.group || *target == entry.cell.member())
        })
        .map(|entry| Arc::clone(&entry.cell))
        .collect()
}

fn admission_replaces_ticket(admission: &SyncAdmission, member: BeatGridId) -> bool {
    match admission {
        SyncAdmission::Prepared(preparation) => preparation.stamp().member().grid_id() == member,
        SyncAdmission::StateChanged { transition, .. }
        | SyncAdmission::TopologyChanged { transition, .. } => {
            transition_replaces_ticket(transition, member)
        }
        _ => false,
    }
}

fn transition_replaces_ticket(transition: &SyncTransition, member: BeatGridId) -> bool {
    transition
        .issued()
        .iter()
        .any(|preparation| preparation.stamp().member().grid_id() == member)
        || transition
            .withdrawn()
            .iter()
            .any(|stamp| stamp.member().grid_id() == member)
}

fn topology_conflicts_with_graph<T, S>(
    state: &SessionState<T, S>,
    operation: &SyncOperation<PlayerMember>,
) -> bool {
    let SyncOperation::Topology { operations, .. } = operation else {
        return false;
    };
    operations.iter().any(|operation| match operation {
        TopologyOperation::Attach { member } => state.graph.index_by_grid(member.id()).is_some(),
        TopologyOperation::Detach { member } => state.graph.index_by_grid(*member).is_some(),
        TopologyOperation::Replace {
            member,
            replacement,
        } => {
            state.graph.index_by_grid(*member).is_some()
                || state.graph.index_by_grid(replacement.id()).is_some()
        }
    })
}

pub(crate) fn run_cmd<T, S>(state: &mut SessionState<T, S>, cmd: Cmd<S>) -> Reply
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    match cmd {
        Cmd::RegisterSyncMember { group, member } => state
            .register_sync_member(group, member)
            .map_or_else(Reply::Err, Reply::SyncGate),
        Cmd::RegisterPlayer {
            grid_id,
            bus,
            eq_layout,
            gate_smoothing,
            pools,
            sample_rate,
        } => match register_player(
            state,
            grid_id,
            bus,
            eq_layout,
            pools,
            sample_rate,
            gate_smoothing,
        ) {
            Ok(player_id) => Reply::PlayerRegistered(player_id),
            Err(error) => Reply::Err(error),
        },
        Cmd::UnregisterPlayer { player_id } => match unregister_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StartPlayer {
            master_volume,
            player_id,
            render_quantum_frames,
            response_budget_frames,
            sample_rate,
        } => match lifecycle::start_player(
            state,
            player_id,
            sample_rate,
            master_volume,
            render_quantum_frames,
            response_budget_frames,
        ) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::StopPlayer { player_id } => match lifecycle::stop_player(state, player_id) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::AllocateSlot { player_id } => {
            slots::allocate_slot(state, player_id).unwrap_or_else(Reply::Err)
        }
        Cmd::ReleaseSlot { player_id, slot } => match slots::release_slot(state, player_id, slot) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerMasterVolumes { levels } => {
            match controls::set_player_master_volumes(state, &levels) {
                Ok(()) => Reply::Ok,
                Err(err) => Reply::Err(err),
            }
        }
        Cmd::SetPlayerSlotVolume {
            player_id,
            slot,
            volume,
        } => match controls::set_player_slot_volume(state, player_id, slot, volume) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerEqGain {
            band,
            gain_db,
            player_id,
        } => match controls::set_player_eq_gain(state, player_id, band, gain_db) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetPlayerEqLayout {
            eq_layout,
            player_id,
        } => match controls::set_player_eq_layout(state, player_id, eq_layout) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::EnableMixTap { writer } => {
            let mut outputs = OutputGroup::new();
            outputs.push(writer);
            match tap::enable(state, outputs) {
                Ok(()) => Reply::Ok,
                Err(err) => Reply::Err(err),
            }
        }
        Cmd::DisableMixTap => {
            tap::disable(state);
            Reply::Ok
        }
        Cmd::SetSessionDucking { mode } => {
            controls::set_session_ducking(state, mode);
            Reply::Ok
        }
        Cmd::SetSessionTempo { tempo } => match transport::set_tempo(state, tempo) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SetSessionPlaying { playing } => match transport::set_playing(state, playing) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::SeekSession { target } => match transport::seek(state, target) {
            Ok(()) => Reply::Ok,
            Err(err) => Reply::Err(err),
        },
        Cmd::QuerySessionTransport => match transport::snapshot(state) {
            Ok(snapshot) => Reply::SessionTransport(snapshot),
            Err(err) => Reply::Err(err),
        },
        Cmd::InvalidateAudioRoute { reason } => invalidate_audio_route(state, &reason),
        Cmd::SetSampleRate { sample_rate } => set_sample_rate(state, sample_rate),
        Cmd::QuerySampleRate => {
            trace_stream_info(state, "query-sample-rate");
            Reply::SampleRate(sample_rate(state))
        }
        Cmd::QueryStreamShape => Reply::StreamShape(stream_shape(state)),
        Cmd::Tick => tick_session(state),
        Cmd::AcknowledgeSync { .. } | Cmd::RetireSyncMember { .. } => {
            Reply::Err(SessionError::SyncControlBusy)
        }
    }
}

/// The shape of the stream the session is actually running on, if it is
/// running on one. Firewheel keeps a deactivated context's stream description
/// until the processor comes back, so a session awaiting a restart would
/// otherwise keep reporting the route it has already disowned as measured.
fn measured_stream_shape<T, S>(state: &SessionState<T, S>) -> Option<StreamShape> {
    if state.stream_needs_restart {
        return None;
    }
    state
        .ctx
        .as_ref()
        .and_then(FirewheelContext::stream_info)
        .map(|info| StreamShape::new(info.max_block_frames, info.sample_rate))
}

pub(super) fn sample_rate<T, S>(state: &SessionState<T, S>) -> SessionSampleRate {
    let measured = measured_stream_shape(state).map(|shape| shape.sample_rate.get());
    SessionSampleRate::new(measured, state.sample_rate_hint)
}

pub(super) fn stream_shape<T, S>(state: &SessionState<T, S>) -> Option<StreamShape> {
    measured_stream_shape(state).or_else(|| {
        Some(StreamShape::new(
            state.requested_max_block_frames?,
            NonZeroU32::new(state.sample_rate_hint)?,
        ))
    })
}

pub(super) fn tick_session<T, S>(state: &mut SessionState<T, S>) -> Reply {
    if state.stream_needs_restart {
        match restart_stream(state, state.sample_rate_hint) {
            Ok(()) => {}
            Err(err) => {
                warn!(?err, "[KITHARA-ROUTE] deferred stream restart failed");
                return Reply::Err(SessionError::RestartFailed {
                    reason: "deferred stream restart".into(),
                    r#source: err.to_string(),
                });
            }
        }
        if state.stream_needs_restart {
            return Reply::Ok;
        }
    }

    let update = state.ctx.as_mut().map(FirewheelContext::update);
    if let Some(Err(err)) = update {
        return handle_update_error(state, &err);
    }
    if stream_died(state) {
        return restart_dead_stream(state);
    }
    Reply::Ok
}

#[cfg(any(target_arch = "wasm32", test))]
pub(super) fn drain_host_channel<T, S>(
    state: &mut SessionState<T, S>,
    rx: &mpsc::Receiver<HostCmdMsg<S>>,
    mut observe: impl FnMut(&HostReply),
) where
    S: HasPool<f32> + Send + Sync + 'static,
{
    for msg in rx.try_iter() {
        let reply = run_host_cmd(state, msg.cmd);
        observe(&reply);
        msg.reply_tx.send(reply).ok();
    }

    if let Reply::Err(err) = tick_session(state) {
        warn!(?err, "session tick in host drain failed");
    }
}

fn unregister_player<T, S>(
    state: &mut SessionState<T, S>,
    player_id: PlayerId,
) -> Result<(), SessionError> {
    debug!(player_id, "[KITHARA-ROUTE] unregistering player");
    let idx = player_index(state, player_id)?;
    let started = state
        .graph
        .deck(idx)
        .ok_or_else(|| SessionError::Graph("registered deck is missing".to_owned()))?
        .started;
    if started {
        lifecycle::stop_player(state, player_id)?;
    } else if state.ctx.is_some() {
        lifecycle::shutdown_if_idle(state)?;
    }
    state
        .graph
        .remove(idx)
        .ok_or_else(|| SessionError::Graph("registered deck is missing".to_owned()))?;
    debug!(
        player_id,
        players = state.graph.len(),
        "[KITHARA-ROUTE] player unregistered"
    );
    Ok(())
}

fn apply_mix<T, S>(state: &mut SessionState<T, S>, levels: &[HostLevel]) -> Result<(), PlayError> {
    let mut projected: Vec<PlayerLevel> = Vec::with_capacity(levels.len());
    for (index, &HostLevel { grid_id, level }) in levels.iter().enumerate() {
        if !level.is_finite() || !(0.0..=1.0).contains(&level) {
            return Err(PlayError::MixLevel { level });
        }
        if levels[..index]
            .iter()
            .any(|candidate| candidate.grid_id == grid_id)
        {
            return Err(PlayError::MixDuplicatePlayer);
        }
        if state.root.with_group(grid_id, |_| ()).is_none() {
            return Err(PlayError::MixForeignSession);
        }
        if let Some(deck_index) = state.graph.index_by_grid(grid_id) {
            let player_id = state
                .graph
                .deck(deck_index)
                .ok_or_else(|| PlayError::Internal("projected player is missing".into()))?
                .player_id;
            projected.push(PlayerLevel::new(player_id, level));
        }
    }

    controls::set_player_master_volumes(state, &projected)?;
    for &HostLevel { grid_id, level } in levels {
        let updated = state.root.with_group(grid_id, |member| {
            member.commit_host_level(level);
        });
        if updated.is_none() {
            return Err(PlayError::MixForeignSession);
        }
    }
    Ok(())
}

pub(super) fn handle_update_error<T, S>(
    _state: &mut SessionState<T, S>,
    err: &UpdateError,
) -> Reply {
    warn!(?err, "[KITHARA-ROUTE] firewheel update failed");
    Reply::Err(SessionError::Graph(format!("{err:?}")))
}

/// A context that went inactive under a session that believes its stream is
/// running lost that stream: Firewheel hands the processor back when it stops,
/// and since 0.14 that is the only place the death shows up — it is no longer
/// reported as an update error.
pub(super) fn stream_died<T, S>(state: &SessionState<T, S>) -> bool {
    !state.stream_needs_restart && state.ctx.as_ref().is_some_and(|ctx| !ctx.is_active())
}

fn restart_dead_stream<T, S>(state: &mut SessionState<T, S>) -> Reply {
    state.stream_needs_restart = true;
    state.publish_root();
    warn!("session stream stopped unexpectedly; restarting audio stream");
    trace!(
        sample_rate_hint = state.sample_rate_hint,
        "[KITHARA-ROUTE] firewheel context went inactive under a live stream"
    );
    match restart_stream(state, state.sample_rate_hint) {
        Ok(()) => Reply::Ok,
        Err(restart_err) => Reply::Err(SessionError::RestartFailed {
            reason: "audio stream stopped".to_owned(),
            r#source: restart_err.to_string(),
        }),
    }
}

pub(super) fn invalidate_audio_route<T, S>(state: &mut SessionState<T, S>, reason: &str) -> Reply {
    debug!(
        reason,
        ctx_ready = state.ctx.is_some(),
        stream_needs_restart = state.stream_needs_restart,
        "[KITHARA-ROUTE] audio route invalidated"
    );
    if state.ctx.is_none() {
        return Reply::Ok;
    }
    state.stream_needs_restart = true;
    match restart_stream(state, state.sample_rate_hint) {
        Ok(()) => Reply::Ok,
        Err(err) => Reply::Err(SessionError::RestartFailed {
            reason: reason.to_owned(),
            r#source: err.to_string(),
        }),
    }
}

/// Moves the output to `sample_rate` through the same restart a route change takes.
fn set_sample_rate<T, S>(state: &mut SessionState<T, S>, sample_rate: NonZeroU32) -> Reply {
    state.sample_rate_hint = sample_rate.get();
    invalidate_audio_route(state, "sample rate change")
}

pub(super) fn restart_stream<T, S>(
    state: &mut SessionState<T, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    if state.ctx.is_none() {
        return Err(SessionError::NoContext);
    }
    debug!(sample_rate, "[KITHARA-ROUTE] restarting firewheel stream");
    if transport::prepare_route_restart(state, sample_rate)? == RouteRestartStatus::Pending {
        trace!("[KITHARA-ROUTE] waiting for the previous stream processor to stop");
        return Ok(());
    }
    let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let stream = (state.start_stream_fn)(fw_ctx, sample_rate).map_err(SessionError::StreamStart)?;
    state.stream = Some(stream);
    state.reserved_session_grid = None;
    state.sample_rate_hint = sample_rate;
    state.stream_needs_restart = false;
    state.publish_root();
    trace_stream_info(state, "restart-stream");
    debug!(
        sample_rate,
        "[KITHARA-ROUTE] firewheel stream restart complete"
    );
    Ok(())
}

pub(super) fn trace_stream_info<T, S>(state: &SessionState<T, S>, context: &'static str) {
    if let Some(info) = state.ctx.as_ref().and_then(FirewheelContext::stream_info) {
        trace!(
            context,
            sample_rate = info.sample_rate.get(),
            prev_sample_rate = info.prev_sample_rate.get(),
            max_block_frames = info.max_block_frames.get(),
            out_channels = info.num_stream_out_channels,
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info"
        );
    } else {
        trace!(
            context,
            sample_rate_hint = state.sample_rate_hint,
            requested_max_block_frames = state.requested_max_block_frames.map(NonZeroU32::get),
            stream_needs_restart = state.stream_needs_restart,
            "[KITHARA-ROUTE] session stream-info unavailable"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroUsize},
        sync::atomic::AtomicBool,
    };

    use firewheel::{ActivateInfo, processor::FirewheelProcessor};
    use kithara_events::EventBus;
    use kithara_output::OutputGroup;
    use kithara_platform::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use kithara_play::DEFAULT_GATE_SMOOTHING;
    use kithara_sync::SyncGroupSnapshot;
    use kithara_test_utils::{
        bufpool::{TestPools, pools},
        kithara,
    };
    use kithara_warp::{BeatGrid, BeatGridSnapshot, BeatGridState, BeatGridUnavailable, MapAxis};
    use ringbuf::{HeapRb, traits::Split};

    use super::*;
    use crate::{
        bridge::MixTapWriter,
        session::{
            graph::master_gain,
            protocol::{Cmd, Reply, SessionError},
            state::{Deck, MixTap, SessionState},
            tests::graph::{attach_player, state as test_state},
        },
    };

    #[derive(Default)]
    struct RouteLossProbe {
        fail_next_start: AtomicBool,
        start_count: AtomicUsize,
    }

    impl RouteLossProbe {
        fn reset(&self) {
            self.start_count.store(0, Ordering::SeqCst);
            self.fail_next_start.store(false, Ordering::SeqCst);
        }
    }

    thread_local! {
        static ROUTE_LOSS: RouteLossProbe = RouteLossProbe::default();
    }

    fn route_loss<R>(f: impl FnOnce(&RouteLossProbe) -> R) -> R {
        ROUTE_LOSS.with(f)
    }

    /// The fixture stream. Holding the processor is the whole of it: dropping
    /// this is what a lost audio stream looks like to the context, so a test
    /// simulates the loss by dropping `state.stream`.
    struct RouteLossStream {
        _processor: FirewheelProcessor,
    }

    type TestState = SessionState<RouteLossStream, TestPools>;

    #[derive(Debug, thiserror::Error)]
    #[error("route lost")]
    struct RouteLossError;

    fn start_route_loss_stream(
        ctx: &mut FirewheelContext,
        sample_rate: u32,
    ) -> Result<RouteLossStream, String> {
        route_loss(|probe| probe.start_count.fetch_add(1, Ordering::SeqCst));
        if route_loss(|probe| probe.fail_next_start.swap(false, Ordering::SeqCst)) {
            return Err(RouteLossError.to_string());
        }
        let sample_rate = NonZeroU32::new(sample_rate).unwrap_or(
            NonZeroU32::new(TestState::DEFAULT_SAMPLE_RATE)
                .expect("invariant: fixture default sample rate is non-zero"),
        );
        let max_block_frames =
            NonZeroU32::new(512).expect("invariant: fixture block size is non-zero");
        let processor = ctx
            .activate(ActivateInfo {
                sample_rate,
                max_block_frames,
                num_stream_in_channels: 0,
                num_stream_out_channels: 2,
                input_to_output_latency_seconds: 0.0,
            })
            .map_err(|err| err.to_string())?;
        Ok(RouteLossStream {
            _processor: processor,
        })
    }

    fn register_command(grid_id: BeatGridId, sample_rate: u32) -> Cmd<TestPools> {
        Cmd::RegisterPlayer {
            grid_id,
            sample_rate,
            bus: EventBus::default(),
            eq_layout: Vec::new(),
            gate_smoothing: DEFAULT_GATE_SMOOTHING,
            pools: pools(),
        }
    }

    fn register_player(state: &mut TestState) -> u64 {
        let grid_id = attach_player(state);
        match run_cmd(
            state,
            register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE),
        ) {
            Reply::PlayerRegistered(registered) => registered.id,
            Reply::Err(err) => panic!("player registration failed: {err}"),
            _ => panic!("player registration returned unexpected reply"),
        }
    }

    fn start_command(player_id: u64, sample_rate: u32) -> Cmd<TestPools> {
        Cmd::StartPlayer {
            player_id,
            sample_rate,
            master_volume: 1.0,
            render_quantum_frames: None,
            response_budget_frames: NonZeroUsize::new(448),
        }
    }

    fn deck(state: &TestState, index: usize) -> &Deck<TestPools> {
        state
            .graph
            .deck(index)
            .expect("the registered deck is present under the host")
    }

    fn deck_count(state: &TestState) -> usize {
        state.graph.len()
    }

    fn member_count(state: &TestState) -> usize {
        state
            .root
            .topology()
            .expect("the host topology remains valid")
            .members()
            .len()
    }

    fn host_grid(state: &TestState) -> BeatGridSnapshot {
        state.root.snapshot()
    }

    fn assert_route_boundary(before: &BeatGridSnapshot, boundary: &BeatGridSnapshot) {
        assert_eq!(
            boundary.state(),
            BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
        );
        assert!(boundary.revision() > before.revision());
        let MapAxis::Session(before_axis) = before.axis() else {
            panic!("the previous host grid uses the session axis")
        };
        let MapAxis::Session(boundary_axis) = boundary.axis() else {
            panic!("the route boundary uses the session axis")
        };
        assert!(boundary_axis.epoch() > before_axis.epoch());
    }

    fn deck_by_player_id(state: &TestState, player_id: u64) -> &Deck<TestPools> {
        let index = state
            .graph
            .index_by_player(player_id)
            .expect("the player has a registered deck");
        state
            .graph
            .deck(index)
            .expect("the registered deck is present")
    }

    #[kithara::test]
    fn registration_projects_the_canonical_member_grid() {
        route_loss(RouteLossProbe::reset);
        let mut state = test_state(start_route_loss_stream);
        let host_id = state.root.id();

        let player_id = register_player(&mut state);
        let registered = state.root.topology().expect("the host topology is valid");
        let deck = deck_by_player_id(&state, player_id);
        assert_eq!(registered.members().len(), 1);
        assert_eq!(registered.members()[0].grid().id(), deck.grid_id);
        assert!(registered.members()[0].group_topology().is_some());

        assert!(matches!(
            run_cmd(
                &mut state,
                start_command(player_id, TestState::DEFAULT_SAMPLE_RATE),
            ),
            Reply::Ok
        ));
        assert!(deck_by_player_id(&state, player_id).started);
        let started = state
            .root
            .topology()
            .expect("the host topology remains valid");
        assert_eq!(started.stamp(), registered.stamp());
        // The stream may open a new session epoch at any time; each deck
        // grid descends onto the host's axis without a topology change, so
        // members are compared by identity and by the axis they follow.
        let identity = |topology: &SyncGroupSnapshot| {
            topology
                .members()
                .iter()
                .map(|member| {
                    assert_eq!(member.grid().axis(), topology.group_grid().axis());
                    (
                        member.grid().id(),
                        member.group_topology().map(SyncGroupSnapshot::stamp),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(identity(&started), identity(&registered));

        assert!(matches!(
            run_cmd(&mut state, Cmd::UnregisterPlayer { player_id }),
            Reply::Ok
        ));

        assert_eq!(state.root.id(), host_id);
        let retained = state
            .root
            .topology()
            .expect("the canonical member outlives its graph projection");
        assert_eq!(retained.stamp(), started.stamp());
        assert_eq!(identity(&retained), identity(&started));
        assert_eq!(deck_count(&state), 0);
    }

    #[kithara::test]
    fn registration_rejects_a_player_before_canonical_attachment() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = BeatGridId::allocate().expect("fixture player grid id");

        let reply = run_cmd(
            &mut state,
            register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE),
        );

        assert!(matches!(reply, Reply::Err(SessionError::Graph(_))));
        assert_eq!(member_count(&state), 0);
        assert_eq!(deck_count(&state), 0);
    }

    #[kithara::test]
    fn duplicate_graph_projection_is_rejected() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);
        let command = || register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE);

        assert!(matches!(
            run_cmd(&mut state, command()),
            Reply::PlayerRegistered(_)
        ));
        let next_player_id = state.next_player_id;
        assert!(matches!(
            run_cmd(&mut state, command()),
            Reply::Err(SessionError::Graph(_))
        ));

        assert_eq!(state.next_player_id, next_player_id);
        assert_eq!(member_count(&state), 1);
        assert_eq!(deck_count(&state), 1);
    }

    #[kithara::test]
    fn detach_is_rejected_while_the_graph_projection_is_live() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);
        let Reply::PlayerRegistered(registered) = run_cmd(
            &mut state,
            register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE),
        ) else {
            panic!("fixture player is registered")
        };
        let player_id = registered.id;
        let detach = |state: &TestState| SyncOperation::Topology {
            base: state.root.topology().expect("fixture topology").stamp(),
            operations: Box::new([TopologyOperation::Detach { member: grid_id }]),
        };

        let operation = detach(&state);
        let HostReply::Admission(Err(rejected)) =
            run_host_cmd(&mut state, HostCmd::Sync(SyncCmd::Transact(operation)))
        else {
            panic!("live graph projection rejects canonical detach")
        };
        let (error, _) = <(SyncError, SyncOperation<PlayerMember>)>::from(rejected);
        assert_eq!(
            error,
            SyncError::CapabilityUnavailable {
                capability: SyncCapability::Topology,
            }
        );
        assert_eq!(member_count(&state), 1);
        assert_eq!(deck_count(&state), 1);

        assert!(matches!(
            run_cmd(&mut state, Cmd::UnregisterPlayer { player_id }),
            Reply::Ok
        ));
        let operation = detach(&state);
        assert!(matches!(
            run_host_cmd(&mut state, HostCmd::Sync(SyncCmd::Transact(operation))),
            HostReply::Admission(Ok(SyncAdmission::TopologyChanged { .. }))
        ));
        assert_eq!(member_count(&state), 0);
        assert_eq!(deck_count(&state), 0);
    }

    #[kithara::test]
    fn public_transactions_cannot_withdraw_a_live_slot() {
        let mut state = test_state(start_route_loss_stream);
        let player_id = register_player(&mut state);
        assert!(matches!(
            run_cmd(
                &mut state,
                start_command(player_id, TestState::DEFAULT_SAMPLE_RATE),
            ),
            Reply::Ok
        ));
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        let group = deck_by_player_id(&state, player_id).grid_id;
        let member = state
            .root
            .with_group(group, |group| {
                group.topology().expect("deck topology").members()[0]
                    .grid()
                    .id()
            })
            .expect("registered deck group");
        let prior = state.root.with_group(group, SyncGroup::status);

        let HostReply::Admission(Err(rejected)) = run_host_cmd(
            &mut state,
            HostCmd::Sync(SyncCmd::Transact(SyncOperation::WithdrawQuiescedMember {
                target: member,
            })),
        ) else {
            panic!("public withdrawal is refused while the slot is live");
        };
        assert_eq!(
            rejected.error(),
            &SyncError::QuiescenceRequired { member_id: member }
        );
        assert_eq!(state.root.with_group(group, SyncGroup::status), prior);
        assert_eq!(deck_by_player_id(&state, player_id).slots.len(), 1);
    }

    #[kithara::test]
    fn owner_side_topology_commands_resolve_the_base_when_executed() {
        let mut state = test_state(start_route_loss_stream);
        let first = attach_player(&mut state);
        let second = attach_player(&mut state);
        let before = state.root.topology().expect("fixture topology").stamp();
        let detach = |member| {
            HostCmd::Sync(SyncCmd::TransactCurrent(Box::new([
                TopologyOperation::Detach { member },
            ])))
        };

        assert!(matches!(
            run_host_cmd(&mut state, detach(first)),
            HostReply::Admission(Ok(SyncAdmission::TopologyChanged { .. }))
        ));
        let after_first = state.root.topology().expect("updated topology").stamp();
        assert_ne!(after_first, before);
        assert!(matches!(
            run_host_cmd(&mut state, detach(second)),
            HostReply::Admission(Ok(SyncAdmission::TopologyChanged { .. }))
        ));

        let after_second = state.root.topology().expect("updated topology");
        assert_ne!(after_second.stamp(), after_first);
        assert!(after_second.members().is_empty());
        assert_eq!(state.root_view.topology(), Ok(after_second));
    }

    #[kithara::test]
    fn root_view_publishes_the_canonical_topology() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);

        let topology = state.root.topology().expect("canonical topology");
        let published = state.root_view.topology().expect("published topology");

        assert_eq!(published, topology);
        assert_eq!(published.members().len(), 1);
        assert_eq!(published.members()[0].grid().id(), grid_id);
    }

    #[kithara::test]
    fn invalid_registration_preserves_the_canonical_root() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);
        let next_player_id = state.next_player_id;
        let topology = state.root.topology().expect("fixture topology");

        let reply = run_cmd(&mut state, register_command(grid_id, 0));

        assert!(matches!(
            reply,
            Reply::Err(SessionError::InvalidSampleRate(0))
        ));
        assert_eq!(state.next_player_id, next_player_id);
        assert_eq!(deck_count(&state), 0);
        assert_eq!(state.root.topology().expect("fixture topology"), topology);
        assert!(state.reserved_session_grid.is_some());
    }

    #[kithara::test]
    fn exhausted_player_identity_preserves_the_canonical_root() {
        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);
        let topology = state.root.topology().expect("fixture topology");
        state.next_player_id = u64::MAX;

        let reply = run_cmd(
            &mut state,
            register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE),
        );

        assert!(matches!(reply, Reply::Err(SessionError::PlayerIdExhausted)));
        assert_eq!(state.next_player_id, u64::MAX);
        assert_eq!(deck_count(&state), 0);
        assert_eq!(state.root.topology().expect("fixture topology"), topology);
        assert!(state.reserved_session_grid.is_some());
    }

    #[kithara::test]
    fn sample_rate_query_separates_the_measured_stream_from_the_request() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let Reply::SampleRate(before) = run_cmd(&mut state, Cmd::QuerySampleRate) else {
            panic!("the sample-rate query answers with a sample rate");
        };
        assert_eq!(
            before.measured, None,
            "a session with no stream has measured nothing"
        );
        assert_eq!(
            before.output(),
            TestState::DEFAULT_SAMPLE_RATE,
            "until a stream exists the resampler is built for the requested rate"
        );

        let player_id = register_player(&mut state);
        assert!(matches!(
            run_cmd(&mut state, Cmd::QuerySampleRate),
            Reply::SampleRate(SessionSampleRate {
                measured: None,
                requested: TestState::DEFAULT_SAMPLE_RATE,
                ..
            })
        ));
        assert!(matches!(
            run_cmd(&mut state, start_command(player_id, 48_000),),
            Reply::Ok
        ));
        assert!(matches!(
            run_cmd(&mut state, Cmd::QuerySampleRate),
            Reply::SampleRate(SessionSampleRate {
                measured: Some(48_000),
                requested: 48_000,
                ..
            })
        ));
    }

    #[kithara::test]
    fn stream_shape_query_prefers_measurement_over_an_explicit_request() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        assert!(matches!(
            run_cmd(&mut state, Cmd::QueryStreamShape),
            Reply::StreamShape(None)
        ));

        state.requested_max_block_frames = NonZeroU32::new(128);
        let Reply::StreamShape(Some(requested)) = run_cmd(&mut state, Cmd::QueryStreamShape) else {
            panic!("the explicit output block is available before stream start")
        };
        assert_eq!(requested.max_block_frames.get(), 128);
        assert_eq!(requested.sample_rate.get(), TestState::DEFAULT_SAMPLE_RATE);

        let player_id = register_player(&mut state);
        assert!(matches!(
            run_cmd(
                &mut state,
                start_command(player_id, TestState::DEFAULT_SAMPLE_RATE),
            ),
            Reply::Ok
        ));
        let Reply::StreamShape(Some(measured)) = run_cmd(&mut state, Cmd::QueryStreamShape) else {
            panic!("the running stream reports its measured output shape")
        };
        assert_eq!(measured.max_block_frames.get(), 512);
        assert_eq!(measured.sample_rate.get(), TestState::DEFAULT_SAMPLE_RATE);
        assert_eq!(state.root_view.stream_shape(), Some(measured));
        restart_stream(&mut state, 48_000).expect("restart stream");
        assert_eq!(
            state
                .root_view
                .stream_shape()
                .expect("published shape")
                .sample_rate
                .get(),
            48_000
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::StopPlayer { player_id }),
            Reply::Ok
        ));
        let stopped = state
            .root_view
            .stream_shape()
            .expect("configured shape after stop");
        assert_eq!(stopped.max_block_frames.get(), 128);
        assert_eq!(stopped.sample_rate.get(), 48_000);
    }

    #[kithara::test]
    fn measured_output_block_rejects_player_before_graph_start() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        state.requested_max_block_frames = NonZeroU32::new(128);
        let player_id = register_player(&mut state);
        let command = Cmd::StartPlayer {
            player_id,
            master_volume: 1.0,
            render_quantum_frames: NonZeroUsize::new(64),
            response_budget_frames: NonZeroUsize::new(441),
            sample_rate: TestState::DEFAULT_SAMPLE_RATE,
        };

        assert!(matches!(
            run_cmd(&mut state, command),
            Reply::Err(SessionError::ResponseBudgetExceeded {
                max_block_frames: 512,
                render_quantum_frames: 64,
                required_frames: 639,
                budget_frames: 441,
            })
        ));
        assert!(!deck_by_player_id(&state, player_id).started);
    }

    #[kithara::test]
    fn explicit_audio_route_invalidation_restarts_stream_without_backend_error() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(&mut state, start_command(player_id, 0),),
            Reply::Ok
        ));
        assert!(matches!(
            run_cmd(&mut state, Cmd::QuerySampleRate),
            Reply::SampleRate(SessionSampleRate {
                measured: Some(44_100),
                requested: 0,
                ..
            })
        ));
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );
        let before_route = host_grid(&state);

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::InvalidateAudioRoute {
                    reason: String::from("oldDeviceUnavailable"),
                },
            ),
            Reply::Ok
        ));

        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2,
            "explicit platform route invalidation must restart the audio stream"
        );
        assert_route_boundary(&before_route, &host_grid(&state));
        let first_boundary = host_grid(&state);
        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::InvalidateAudioRoute {
                    reason: String::from("newDeviceAvailable"),
                },
            ),
            Reply::Ok
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            3,
            "a second physical route invalidation must start a new stream generation"
        );
        assert_route_boundary(&first_boundary, &host_grid(&state));
        assert!(
            state.ctx.is_some(),
            "route invalidation must keep the graph context"
        );
        assert!(
            deck(&state, 0).started,
            "route invalidation must keep the player graph logically started"
        );
        assert_eq!(
            deck(&state, 0).slots.len(),
            1,
            "route invalidation must not drop active slots"
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            deck(&state, 0).slots.len(),
            2,
            "session must accept future slots after explicit route restart"
        );
        assert!(!state.stream_needs_restart);
    }

    #[kithara::test]
    fn unexpected_stream_stop_restarts_stream_without_dropping_player_graph_or_future_slots() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(&mut state, start_command(player_id, 0),),
            Reply::Ok
        ));
        assert!(state.ctx.is_some());
        assert!(deck(&state, 0).started);
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(deck(&state, 0).slots.len(), 1);
        let before_route = host_grid(&state);

        state.stream = None;
        assert!(matches!(run_cmd(&mut state, Cmd::Tick), Reply::Ok));

        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2,
            "stream loss must restart the audio stream immediately"
        );
        assert_route_boundary(&before_route, &host_grid(&state));
        assert!(
            state.ctx.is_some(),
            "session must keep the graph context across stream restart"
        );
        assert!(
            state.session_output_node_id.is_some(),
            "session output node id must survive stream restart"
        );
        assert!(
            deck(&state, 0).started,
            "player graph must remain logically started after stream restart"
        );
        assert_eq!(
            deck(&state, 0).slots.len(),
            1,
            "active slot graph must survive stream restart"
        );
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id }),
            Reply::SlotAllocated(..)
        ));
        assert_eq!(
            deck(&state, 0).slots.len(),
            2,
            "session must accept a future slot after route-loss reinit"
        );
        assert!(!state.stream_needs_restart);
    }

    #[kithara::test]
    fn stream_loss_seen_while_draining_host_commands_restarts_the_stream() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(&mut state, start_command(player_id, 44_100)),
            Reply::Ok
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );

        let (_tx, rx) = mpsc::channel::<HostCmdMsg<TestPools>>();

        state.stream = None;
        drain_host_channel(&mut state, &rx, |_| {});

        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2,
            "a stream drop observed during a host command drain must restart the stream"
        );
        assert!(!state.stream_needs_restart);
    }

    #[kithara::test]
    fn failed_stream_restart_is_retried_on_next_tick() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let player_id = register_player(&mut state);

        assert!(matches!(
            run_cmd(&mut state, start_command(player_id, 44_100),),
            Reply::Ok
        ));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            1
        );
        let before_route = host_grid(&state);

        state.stream = None;
        route_loss(|probe| probe.fail_next_start.store(true, Ordering::SeqCst));
        match run_cmd(&mut state, Cmd::Tick) {
            Reply::Err(err) => assert!(
                matches!(err, SessionError::RestartFailed { .. }),
                "restart failure must be surfaced, got {err:?}"
            ),
            _ => panic!("failed restart must return Reply::Err"),
        }

        assert!(
            state.stream_needs_restart,
            "a failed restart must leave retry state armed"
        );
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            2
        );
        let boundary = host_grid(&state);
        assert_route_boundary(&before_route, &boundary);

        assert!(matches!(run_cmd(&mut state, Cmd::Tick), Reply::Ok));
        assert_eq!(
            route_loss(|probe| probe.start_count.load(Ordering::SeqCst)),
            3,
            "next tick must retry the stream restart"
        );
        let retried = host_grid(&state);
        assert_eq!(retried.stamp(), boundary.stamp());
        assert_eq!(retried.axis(), boundary.axis());
        assert!(!state.stream_needs_restart);
        assert!(deck(&state, 0).started);
    }

    fn start_player_cmd(state: &mut TestState, player_id: u64) {
        assert!(matches!(
            run_cmd(&mut *state, start_command(player_id, 44_100),),
            Reply::Ok
        ));
    }

    fn master_volume_of(state: &TestState, player_id: u64) -> f32 {
        state
            .graph
            .decks()
            .find(|player| player.player_id == player_id)
            .expect("player present")
            .master_volume
    }

    fn apply_player_mix(
        state: &mut TestState,
        levels: impl IntoIterator<Item = (u64, f32)>,
    ) -> HostReply {
        let levels = levels
            .into_iter()
            .map(|(player_id, level)| {
                HostLevel::new(deck_by_player_id(state, player_id).grid_id, level)
            })
            .collect();
        run_host_cmd(state, HostCmd::ApplyMix { levels })
    }

    #[kithara::test]
    fn host_mix_before_registration_becomes_the_start_level() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let grid_id = attach_player(&mut state);
        assert!(matches!(
            run_host_cmd(
                &mut state,
                HostCmd::ApplyMix {
                    levels: Box::new([HostLevel::new(grid_id, 0.4)]),
                },
            ),
            HostReply::Ok
        ));
        let Reply::PlayerRegistered(registered) = run_cmd(
            &mut state,
            register_command(grid_id, TestState::DEFAULT_SAMPLE_RATE),
        ) else {
            panic!("player registration must succeed")
        };
        let player_id = registered.id;

        start_player_cmd(&mut state, player_id);

        assert_eq!(master_volume_of(&state, player_id), 0.4);
        assert_eq!(
            deck_by_player_id(&state, player_id)
                .master_volume_memo
                .as_ref()
                .expect("started player has a volume node")
                .volume,
            master_gain(0.4),
        );
    }

    #[kithara::test]
    fn host_mix_updates_one_two_and_four_players_together() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let ids: Vec<u64> = (0..4).map(|_| register_player(&mut state)).collect();
        for &id in &ids {
            start_player_cmd(&mut state, id);
        }

        assert!(matches!(
            apply_player_mix(&mut state, [(ids[0], 0.1)]),
            HostReply::Ok
        ));
        assert_eq!(master_volume_of(&state, ids[0]), 0.1);

        assert!(matches!(
            apply_player_mix(&mut state, [(ids[1], 0.2), (ids[2], 0.3)]),
            HostReply::Ok
        ));
        assert_eq!(master_volume_of(&state, ids[1]), 0.2);
        assert_eq!(master_volume_of(&state, ids[2]), 0.3);
        assert_eq!(master_volume_of(&state, ids[3]), 1.0);

        assert!(matches!(
            apply_player_mix(
                &mut state,
                [(ids[0], 0.4), (ids[1], 0.5), (ids[2], 0.6), (ids[3], 0.7),],
            ),
            HostReply::Ok
        ));
        assert_eq!(master_volume_of(&state, ids[0]), 0.4);
        assert_eq!(master_volume_of(&state, ids[3]), 0.7);
    }

    #[kithara::test]
    fn host_mix_rejects_duplicate_player_without_mutation() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let id = register_player(&mut state);
        start_player_cmd(&mut state, id);

        assert!(matches!(
            apply_player_mix(&mut state, [(id, 0.3), (id, 0.4)]),
            HostReply::Err(PlayError::MixDuplicatePlayer)
        ));
        assert_eq!(master_volume_of(&state, id), 1.0);
    }

    #[kithara::test]
    fn host_mix_rejects_invalid_level_without_mutation() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let a = register_player(&mut state);
        let b = register_player(&mut state);
        start_player_cmd(&mut state, a);
        start_player_cmd(&mut state, b);

        for bad in [f32::NAN, f32::INFINITY, 1.5, -0.1] {
            assert!(matches!(
                apply_player_mix(&mut state, [(a, 0.5), (b, bad)]),
                HostReply::Err(PlayError::MixLevel { .. })
            ));
            assert_eq!(
                master_volume_of(&state, a),
                1.0,
                "level {bad} leaked a mutation"
            );
            assert_eq!(master_volume_of(&state, b), 1.0);
        }
    }

    #[kithara::test]
    fn host_mix_rejects_foreign_member_leaving_known_unchanged() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let known = register_player(&mut state);
        start_player_cmd(&mut state, known);
        let known_grid = deck_by_player_id(&state, known).grid_id;
        let unknown_grid = BeatGridId::allocate().expect("foreign fixture grid id");

        assert!(matches!(
            run_host_cmd(
                &mut state,
                HostCmd::ApplyMix {
                    levels: Box::new([
                        HostLevel::new(known_grid, 0.2),
                        HostLevel::new(unknown_grid, 0.3),
                    ]),
                },
            ),
            HostReply::Err(PlayError::MixForeignSession)
        ));
        assert_eq!(master_volume_of(&state, known), 1.0);
    }

    fn mix_tap_writer(drops: &Arc<AtomicU64>) -> MixTapWriter {
        const TAP_CAPACITY: usize = 1_024;

        let (pcm, _cons) = HeapRb::<f32>::new(TAP_CAPACITY).split();
        MixTapWriter::new(pcm, Arc::clone(drops))
    }

    #[kithara::test]
    fn output_group_is_one_tap_and_is_cleared_by_idle_teardown() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let id = register_player(&mut state);
        start_player_cmd(&mut state, id);

        let drops = Arc::new(AtomicU64::new(0));
        let mut outputs = OutputGroup::new();
        outputs.push(mix_tap_writer(&drops));
        outputs.push(mix_tap_writer(&drops));
        assert!(matches!(
            run_host_cmd(&mut state, HostCmd::EnableOutput { outputs },),
            HostReply::Ok
        ));
        assert!(
            matches!(state.mix_tap, Some(MixTap::Installed(_))),
            "a tap armed on a running session reaches the graph at once"
        );

        assert!(
            matches!(
                run_cmd(
                    &mut state,
                    Cmd::EnableMixTap {
                        writer: mix_tap_writer(&drops),
                    },
                ),
                Reply::Err(SessionError::MixTapActive)
            ),
            "a second consumer must be rejected instead of silently replacing the first"
        );

        assert!(matches!(
            run_cmd(&mut state, Cmd::StopPlayer { player_id: id }),
            Reply::Ok
        ));
        assert!(state.session_limiter_node_id.is_none());
        assert!(
            state.mix_tap.is_none(),
            "idle teardown must clear the mix tap with the context it lived in"
        );
    }

    #[kithara::test]
    fn a_player_attached_after_an_idle_teardown_stops_through_the_next_one() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let first = register_player(&mut state);
        start_player_cmd(&mut state, first);
        assert!(matches!(
            run_cmd(&mut state, Cmd::StopPlayer { player_id: first }),
            Reply::Ok
        ));

        let second = register_player(&mut state);
        start_player_cmd(&mut state, second);
        match run_cmd(&mut state, Cmd::StopPlayer { player_id: second }) {
            Reply::Ok => {}
            Reply::Err(error) => {
                panic!("a player that joined after a route boundary must follow the next: {error}")
            }
            _ => panic!("stop returned an unexpected reply"),
        }
    }

    #[kithara::test]
    fn session_output_has_exactly_one_limiter_rebuilt_on_route_recreate() {
        route_loss(RouteLossProbe::reset);

        let mut state = test_state(start_route_loss_stream);
        let id = register_player(&mut state);
        start_player_cmd(&mut state, id);
        assert!(
            state.session_limiter_node_id.is_some(),
            "limiter node exists after start"
        );

        assert!(matches!(
            run_cmd(&mut state, Cmd::StopPlayer { player_id: id }),
            Reply::Ok
        ));
        assert!(state.session_limiter_node_id.is_none());
        assert!(state.session_output_node_id.is_none());

        start_player_cmd(&mut state, id);
        assert!(
            state.session_limiter_node_id.is_some(),
            "route recreate rebuilds the limiter node"
        );
    }
}
