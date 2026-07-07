use firewheel::{
    FirewheelCtx, Volume, backend::AudioBackend, diff::Memo, node::NodeID,
    nodes::volume_pan::VolumePanNode,
};
use tracing::{debug, warn};

use super::{
    protocol::{AllocatedSlot, PlayerId, Reply, SessionError},
    state::{PlayerState, SessionState, SlotNodes, ensure_ctx},
};
use crate::{
    bridge::slot_channels,
    impls::{master_eq_node::MasterEqNode, player_node::PlayerNode},
    types::{SessionDuckingMode, SlotId},
};
pub(super) fn ducking_gain(mode: SessionDuckingMode) -> f32 {
    match mode {
        SessionDuckingMode::Off => 1.0,
        SessionDuckingMode::Soft => 0.4,
        SessionDuckingMode::Hard => 0.2,
    }
}
pub(super) fn player_index<B: AudioBackend>(
    state: &SessionState<B>,
    player_id: PlayerId,
) -> Result<usize, SessionError> {
    state
        .players
        .iter()
        .position(|player| player.player_id == player_id)
        .ok_or(SessionError::PlayerNotFound(player_id))
}
fn graph_state(message: &'static str) -> SessionError {
    SessionError::Graph(message.into())
}
fn connect_stereo<B: AudioBackend>(
    fw_ctx: &mut FirewheelCtx<B>,
    from: NodeID,
    to: NodeID,
    label: &'static str,
) -> Result<(), SessionError> {
    fw_ctx
        .connect(from, to, &[(0, 0), (1, 1)], false)
        .map(|_| ())
        .map_err(|err| SessionError::Graph(format!("{label} failed: {err}")))
}

pub(super) mod lifecycle {
    use super::*;

    pub(in crate::impls::session) fn start_player<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        sample_rate: u32,
        master_volume: f32,
    ) -> Result<(), SessionError> {
        debug!(
            player_id,
            sample_rate, master_volume, "[KITHARA-ROUTE] starting player"
        );
        ensure_ctx(state, sample_rate)?;
        let idx = player_index(state, player_id)?;
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(session_output_id) = state.session_output_node_id else {
            return Err(graph_state("session output node is not initialised"));
        };
        let player = &mut state.players[idx];
        if player.started {
            return Err(SessionError::AlreadyStarted(player_id));
        }
        let mut master_eq = MasterEqNode::new(&player.eq_layout);
        for (band, gain) in player.shared_eq.snapshot().into_iter().enumerate() {
            master_eq.set_gain(band, gain);
        }
        let master_eq_memo = Memo::new(master_eq.clone());
        let master_eq_id = fw_ctx.add_node(master_eq, None);
        player.master_volume = master_volume.clamp(0.0, 1.0);
        let master_vol = VolumePanNode::from_volume(Volume::Linear(player.master_volume));
        let master_vol_memo = Memo::new(master_vol);
        let master_vol_id = fw_ctx.add_node(master_vol, None);
        let eq_to_volume = "connect player master_eq->master_vol";
        connect_stereo(fw_ctx, master_eq_id, master_vol_id, eq_to_volume)?;
        let volume_to_output = "connect player master_vol->session_output";
        connect_stereo(fw_ctx, master_vol_id, session_output_id, volume_to_output)?;
        if let Err(err) = fw_ctx.update() {
            warn!(player_id, "graph update after player start failed: {err:?}");
        }
        player.master_eq_node_id = Some(master_eq_id);
        player.master_eq_memo = Some(master_eq_memo);
        player.master_vol_pan_node_id = Some(master_vol_id);
        player.master_vol_pan_memo = Some(master_vol_memo);
        player.started = true;
        debug!(
            player_id,
            ?master_eq_id,
            ?master_vol_id,
            "[KITHARA-ROUTE] player graph started"
        );
        Ok(())
    }
    pub(in crate::impls::session) fn stop_player<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
    ) -> Result<(), SessionError> {
        debug!(player_id, "[KITHARA-ROUTE] stopping player");
        let idx = player_index(state, player_id)?;
        stop_player_idx(state, idx)
    }
    fn stop_player_idx<B: AudioBackend>(
        state: &mut SessionState<B>,
        idx: usize,
    ) -> Result<(), SessionError> {
        if idx >= state.players.len() {
            return Err(graph_state("player index out of range"));
        }
        {
            let player = &mut state.players[idx];
            if !player.started {
                return Err(SessionError::NotRunning(player.player_id));
            }
            if let Some(ref mut fw_ctx) = state.ctx {
                remove_player_graph(fw_ctx, player);
                if let Err(err) = fw_ctx.update() {
                    warn!(
                        player_id = player.player_id,
                        "graph update after player stop failed: {err:?}"
                    );
                }
            } else {
                clear_player_graph_state(player);
            }
            player.started = false;
        }
        shutdown_if_idle(state);
        debug!("[KITHARA-ROUTE] player stopped");
        Ok(())
    }
    pub(super) fn remove_player_graph<B: AudioBackend>(
        fw_ctx: &mut FirewheelCtx<B>,
        player: &mut PlayerState,
    ) {
        let player_id = player.player_id;
        let slots = player.slots.drain(..).collect::<Vec<_>>();
        for slot in slots {
            if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
                warn!(player_id, ?err, "failed to remove slot vol_pan node");
            }
            if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
                warn!(player_id, ?err, "failed to remove slot player node");
            }
        }
        if let Some(master_id) = player.master_vol_pan_node_id.take()
            && let Err(err) = fw_ctx.remove_node(master_id)
        {
            warn!(player_id, ?err, "failed to remove player master vol node");
        }
        if let Some(master_eq_id) = player.master_eq_node_id.take()
            && let Err(err) = fw_ctx.remove_node(master_eq_id)
        {
            warn!(player_id, ?err, "failed to remove player master eq node");
        }
        clear_player_graph_state(player);
    }
    pub(super) fn clear_player_graph_state(player: &mut PlayerState) {
        player.master_eq_memo = None;
        player.master_vol_pan_memo = None;
    }
    fn shutdown_if_idle<B: AudioBackend>(state: &mut SessionState<B>) {
        if state.players.iter().all(|player| !player.started) {
            debug!("[KITHARA-ROUTE] shutting down idle session stream");
            if let Some(ref mut fw_ctx) = state.ctx {
                fw_ctx.stop_stream();
            }
            state.ctx = None;
            state.session_output_node_id = None;
            state.session_output_memo = None;
        }
    }
}

pub(super) mod slots {
    use super::*;

    pub(in crate::impls::session) fn allocate_slot<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
    ) -> Result<Reply, SessionError> {
        debug!(player_id, "[KITHARA-ROUTE] allocating player slot");
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let (fw_ctx, master_eq_id) = match (&mut state.ctx, state.players[idx].master_eq_node_id) {
            (None, _) => return Err(SessionError::NoContext),
            (Some(_), None) => return Err(graph_state("player master eq node is not initialised")),
            (Some(fw_ctx), Some(master_eq_id)) => (fw_ctx, master_eq_id),
        };
        let slot_id = SlotId::new(state.players[idx].next_slot_id);
        state.players[idx].next_slot_id += 1;
        let shared_eq = state.players[idx].shared_eq.clone();
        let (inputs, control) = slot_channels(shared_eq);
        let player_node = PlayerNode::new(inputs, state.players[idx].pcm_pool.clone());
        let player_node_id = fw_ctx.add_node(player_node, None);
        let slot_vol_pan = VolumePanNode::from_volume(Volume::Linear(1.0));
        let slot_vol_pan_memo = Memo::new(slot_vol_pan);
        let slot_vol_pan_id = fw_ctx.add_node(slot_vol_pan, None);
        let player_to_slot = "connect player->slot_vol_pan";
        connect_stereo(fw_ctx, player_node_id, slot_vol_pan_id, player_to_slot)?;
        let slot_to_master = "connect slot_vol_pan->player_master_eq";
        connect_stereo(fw_ctx, slot_vol_pan_id, master_eq_id, slot_to_master)?;
        if let Err(err) = fw_ctx.update() {
            warn!(
                player_id,
                ?slot_id,
                "graph update after slot allocate failed: {err:?}"
            );
        }
        state.players[idx].slots.push(SlotNodes {
            slot_id,
            player_node_id,
            vol_pan_memo: slot_vol_pan_memo,
            vol_pan_node_id: slot_vol_pan_id,
        });
        debug!(
            player_id,
            ?slot_id,
            ?player_node_id,
            ?slot_vol_pan_id,
            slots = state.players[idx].slots.len(),
            "[KITHARA-ROUTE] player slot allocated"
        );
        let reply = Reply::SlotAllocated(AllocatedSlot {
            slot: slot_id,
            control,
        });
        Ok(reply)
    }
    pub(in crate::impls::session) fn release_slot<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        slot: SlotId,
    ) -> Result<(), SessionError> {
        debug!(player_id, ?slot, "[KITHARA-ROUTE] releasing player slot");
        let idx = player_index(state, player_id)?;
        let slot_nodes = {
            let player = &mut state.players[idx];
            if !player.started {
                return Err(SessionError::NotRunning(player_id));
            }
            take_slot(player, slot)?
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        remove_slot_graph(fw_ctx, player_id, &slot_nodes);
        debug!(
            player_id,
            ?slot_nodes,
            "[KITHARA-ROUTE] player slot released"
        );
        Ok(())
    }
    pub(super) fn take_slot(
        player: &mut PlayerState,
        slot: SlotId,
    ) -> Result<SlotNodes, SessionError> {
        let Some(slot_idx) = player.slots.iter().position(|s| s.slot_id == slot) else {
            return Err(SessionError::SlotNotFound(slot));
        };
        Ok(player.slots.remove(slot_idx))
    }
    pub(super) fn remove_slot_graph<B: AudioBackend>(
        fw_ctx: &mut FirewheelCtx<B>,
        player_id: PlayerId,
        slot: &SlotNodes,
    ) {
        if let Err(err) = fw_ctx.remove_node(slot.vol_pan_node_id) {
            warn!(player_id, ?err, "failed to remove slot vol_pan node");
        }
        if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
            warn!(player_id, ?err, "failed to remove slot player node");
        }
        if let Err(err) = fw_ctx.update() {
            warn!(player_id, "graph update after slot release failed: {err:?}");
        }
    }
}

pub(super) mod controls {
    use super::*;

    pub(in crate::impls::session) fn set_player_master_volume<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        volume: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        let master_volume = volume.clamp(0.0, 1.0);
        state.players[idx].master_volume = master_volume;
        if !state.players[idx].started {
            return Ok(());
        }
        let player = &mut state.players[idx];
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(master_id) = player.master_vol_pan_node_id else {
            return Err(graph_state("player master vol node is not initialised"));
        };
        let Some(memo) = &mut player.master_vol_pan_memo else {
            return Err(graph_state("player master vol memo is not initialised"));
        };
        memo.volume = Volume::Linear(master_volume);
        let mut queue = fw_ctx.event_queue(master_id);
        memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::impls::session) fn set_player_slot_volume<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let Some(slot_nodes) = state.players[idx]
            .slots
            .iter_mut()
            .find(|s| s.slot_id == slot)
        else {
            return Err(SessionError::SlotNotFound(slot));
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        slot_nodes.vol_pan_memo.volume = Volume::Linear(volume.clamp(0.0, 1.0));
        let mut queue = fw_ctx.event_queue(slot_nodes.vol_pan_node_id);
        slot_nodes.vol_pan_memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::impls::session) fn set_player_eq_gain<B: AudioBackend>(
        state: &mut SessionState<B>,
        player_id: PlayerId,
        band: usize,
        gain_db: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !state.players[idx].started {
            return Err(SessionError::NotRunning(player_id));
        }
        let player = &mut state.players[idx];
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(master_eq_id) = player.master_eq_node_id else {
            return Err(graph_state("player master eq node is not initialised"));
        };
        let Some(memo) = &mut player.master_eq_memo else {
            return Err(graph_state("player master eq memo is not initialised"));
        };
        if band >= memo.bands.len() {
            return Err(SessionError::EqBandOutOfRange {
                band,
                bands: memo.bands.len(),
            });
        }
        memo.set_gain(band, gain_db);
        let mut queue = fw_ctx.event_queue(master_eq_id);
        memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::impls::session) fn set_session_ducking<B: AudioBackend>(
        state: &mut SessionState<B>,
        mode: SessionDuckingMode,
    ) {
        state.session_ducking = mode;
        if let (Some(fw_ctx), Some(session_id), Some(memo)) = (
            &mut state.ctx,
            state.session_output_node_id,
            &mut state.session_output_memo,
        ) {
            memo.volume = Volume::Linear(ducking_gain(mode));
            let mut queue = fw_ctx.event_queue(session_id);
            memo.update_memo(&mut queue);
        }
    }
}
