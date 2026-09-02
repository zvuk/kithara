use std::num::NonZeroUsize;

use firewheel::{
    FirewheelCtx, Volume, backend::AudioBackend, diff::Memo,
    dsp::volume::amp_to_linear_volume_clamped, node::NodeID, nodes::volume::VolumeNode,
};
use kithara_bufpool::HasPool;
use kithara_warp::{BeatGrid, MapAxis};
use tracing::{debug, warn};

use super::{
    protocol::{AllocatedSlot, PlayerId, PlayerLevel, Reply, SessionError},
    state::{Deck, GraphRegistry, MixTap, SessionState, SlotNodes, ensure_ctx, prepare_eq_layout},
    transport::SessionTransportState,
};
use crate::{
    api::{SessionDuckingMode, SlotId},
    bridge::{MixTapWriter, slot_channels},
    effects::eq::{EqBandConfig, EqConfig, GainDb},
    rt::{MasterEqNode, PlayerNode, TapNode},
};
pub(super) const fn ducking_gain(mode: SessionDuckingMode) -> f32 {
    mode.gain()
}
/// A level is a linear amplitude, but `Volume::Linear` is a fader taper that
/// squares its argument, so it must be converted rather than passed through.
pub(super) fn master_gain(level: f32) -> Volume {
    Volume::Linear(amp_to_linear_volume_clamped(level, 0.0))
}
pub(super) fn player_index<B: AudioBackend, S>(
    state: &SessionState<B, S>,
    player_id: PlayerId,
) -> Result<usize, SessionError> {
    state
        .graph
        .index_by_player(player_id)
        .ok_or(SessionError::PlayerNotFound(player_id))
}
fn graph_state(message: &'static str) -> SessionError {
    SessionError::Graph(message.into())
}

fn deck_at<B: AudioBackend, S>(
    state: &SessionState<B, S>,
    index: usize,
) -> Result<&Deck<S>, SessionError> {
    state
        .graph
        .deck(index)
        .ok_or_else(|| graph_state("player index out of range"))
}

fn deck_at_mut<S>(
    graph: &mut GraphRegistry<S>,
    index: usize,
) -> Result<&mut Deck<S>, SessionError> {
    graph
        .deck_mut(index)
        .ok_or_else(|| graph_state("player index out of range"))
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

pub(super) mod tap {
    use super::*;

    pub(in crate::session) fn enable<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        writer: MixTapWriter,
    ) -> Result<(), SessionError> {
        if state.mix_tap.is_some() {
            return Err(SessionError::MixTapActive);
        }
        let Some(limiter_id) = state.session_limiter_node_id else {
            state.mix_tap = Some(MixTap::Requested(writer));
            return Ok(());
        };
        install(state, limiter_id, writer)
    }

    pub(in crate::session) fn disable<B: AudioBackend, S>(state: &mut SessionState<B, S>) {
        let Some(MixTap::Installed(tap_id)) = state.mix_tap.take() else {
            return;
        };
        let Some(ref mut fw_ctx) = state.ctx else {
            return;
        };
        if let Err(err) = fw_ctx.remove_node(tap_id) {
            warn!(?err, "failed to remove session mix tap node");
        }
        if let Err(err) = fw_ctx.update() {
            warn!("graph update after mix tap disable failed: {err:?}");
        }
    }

    pub(in crate::session) fn install_requested<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        limiter_id: NodeID,
    ) -> Result<(), SessionError> {
        let Some(MixTap::Requested(writer)) = state.mix_tap.take() else {
            return Ok(());
        };
        install(state, limiter_id, writer)
    }

    fn install<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        limiter_id: NodeID,
        writer: MixTapWriter,
    ) -> Result<(), SessionError> {
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let tap_id = fw_ctx.add_node(TapNode::new(writer), None);
        if let Err(err) = connect_stereo(fw_ctx, limiter_id, tap_id, "connect limiter->mix_tap") {
            if let Err(remove_err) = fw_ctx.remove_node(tap_id) {
                warn!(?remove_err, "failed to remove the unconnected mix tap node");
            }
            return Err(err);
        }
        if let Err(err) = fw_ctx.update() {
            warn!("graph update after mix tap install failed: {err:?}");
        }
        state.mix_tap = Some(MixTap::Installed(tap_id));
        debug!(?tap_id, "[KITHARA-ROUTE] session mix tap installed");
        Ok(())
    }
}

pub(super) mod lifecycle {
    use super::*;

    pub(in crate::session) fn start_player<B, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
        sample_rate: u32,
        master_volume: f32,
        render_quantum_frames: NonZeroUsize,
        response_budget_frames: NonZeroUsize,
    ) -> Result<(), SessionError>
    where
        B: AudioBackend,
        S: HasPool<f32> + Send + Sync + 'static,
    {
        debug!(
            player_id,
            sample_rate, master_volume, "[KITHARA-ROUTE] starting player"
        );
        ensure_ctx(state, sample_rate)?;
        validate_response_geometry(state, render_quantum_frames, response_budget_frames)?;
        let idx = player_index(state, player_id)?;
        let Some(session_output_id) = state.session_output_node_id else {
            return Err(graph_state("session output node is not initialised"));
        };
        let (ctx, graph) = (&mut state.ctx, &mut state.graph);
        let fw_ctx = ctx.as_mut().ok_or(SessionError::NoContext)?;
        let player = deck_at_mut(graph, idx)?;
        if player.started {
            return Err(SessionError::AlreadyStarted(player_id));
        }
        let eq_config = EqConfig::builder(player.pools.clone()).build();
        let mut master_eq = MasterEqNode::new(eq_config, &player.eq_layout);
        for (band, gain) in player.shared_eq.snapshot().into_iter().enumerate() {
            master_eq.set_gain(band, GainDb::from(gain));
        }
        let master_eq_memo = Memo::new(master_eq.clone());
        let master_eq_id = fw_ctx.add_node(master_eq, None);
        let master_volume = VolumeNode {
            volume: master_gain(player.master_volume),
            ..VolumeNode::default()
        };
        let master_volume_memo = Memo::new(master_volume);
        let master_volume_id = fw_ctx.add_node(master_volume, None);
        let eq_to_volume = "connect player master_eq->master_vol";
        connect_stereo(fw_ctx, master_eq_id, master_volume_id, eq_to_volume)?;
        let volume_to_output = "connect player master_vol->session_output";
        connect_stereo(
            fw_ctx,
            master_volume_id,
            session_output_id,
            volume_to_output,
        )?;
        if let Err(err) = fw_ctx.update() {
            warn!(player_id, "graph update after player start failed: {err:?}");
        }
        player.master_eq_node_id = Some(master_eq_id);
        player.master_eq_memo = Some(master_eq_memo);
        player.master_volume_node_id = Some(master_volume_id);
        player.master_volume_memo = Some(master_volume_memo);
        player.started = true;
        debug!(
            player_id,
            ?master_eq_id,
            ?master_volume_id,
            "[KITHARA-ROUTE] player graph started"
        );
        Ok(())
    }

    fn validate_response_geometry<B: AudioBackend, S>(
        state: &SessionState<B, S>,
        render_quantum_frames: NonZeroUsize,
        response_budget_frames: NonZeroUsize,
    ) -> Result<(), SessionError> {
        let max_block_frames = state
            .ctx
            .as_ref()
            .and_then(FirewheelCtx::stream_info)
            .ok_or(SessionError::NoContext)?
            .max_block_frames
            .get();
        let block_frames = usize::try_from(max_block_frames)
            .map_err(|_| SessionError::ResponseGeometryOverflow)?;
        let quantum_frames = render_quantum_frames.get();
        let preload_chunks = block_frames.div_ceil(quantum_frames);
        let required_frames = preload_chunks
            .checked_add(2)
            .and_then(|chunks| chunks.checked_mul(quantum_frames))
            .and_then(|frames| frames.checked_sub(1))
            .ok_or(SessionError::ResponseGeometryOverflow)?;
        if required_frames > response_budget_frames.get() {
            return Err(SessionError::ResponseBudgetExceeded {
                max_block_frames,
                render_quantum_frames: quantum_frames,
                required_frames,
                budget_frames: response_budget_frames.get(),
            });
        }
        Ok(())
    }
    pub(in crate::session) fn stop_player<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
    ) -> Result<(), SessionError> {
        debug!(player_id, "[KITHARA-ROUTE] stopping player");
        let idx = player_index(state, player_id)?;
        stop_player_idx(state, idx)
    }
    fn stop_player_idx<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        idx: usize,
    ) -> Result<(), SessionError> {
        {
            let (ctx, graph) = (&mut state.ctx, &mut state.graph);
            let player = deck_at_mut(graph, idx)?;
            if !player.started {
                return Err(SessionError::NotRunning(player.player_id));
            }
            if let Some(fw_ctx) = ctx {
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
        shutdown_if_idle(state)?;
        debug!("[KITHARA-ROUTE] player stopped");
        Ok(())
    }
    /// Release the output device once no player is left to feed it. A media
    /// app that has stopped playing must not keep the platform's output
    /// engaged; the next `start_player` builds a fresh context.
    fn shutdown_if_idle<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
    ) -> Result<(), SessionError> {
        let idle = state.graph.decks().all(|deck| !deck.started);
        if idle {
            debug!("[KITHARA-ROUTE] shutting down idle session stream");
            if state.ctx.is_none() {
                return Err(SessionError::NoContext);
            }
            let observed_session_grid = state
                .transport_control
                .as_mut()
                .ok_or_else(|| {
                    graph_state("session transport control is missing during idle shutdown")
                })?
                .observation()
                .session_grid();
            let mut session_grid_generation = match state.reserved_session_grid {
                Some(reserved) => reserved
                    .promote(observed_session_grid)
                    .map_err(|error| graph_state(error.message()))?,
                None => observed_session_grid,
            };
            // Backends may defer processor drop after `stop_stream`. Reserve a
            // successor before stopping so teardown never depends on the RT
            // `stream_stopped` callback reaching this control handle. Control
            // admits at most one unobserved commit; the next context advances
            // once more before publishing.
            session_grid_generation
                .advance_restart()
                .map_err(|error| graph_state(error.message()))?;
            let session_stamp = session_grid_generation
                .stamp()
                .map_err(|error| graph_state(error.message()))?;
            let MapAxis::Session(axis) = state.root.snapshot().axis() else {
                return Err(graph_state(
                    "session host published a non-session grid during idle shutdown",
                ));
            };
            let sample_rate = axis.sample_rate();
            state.root.publish_unavailable_grid(
                session_stamp,
                sample_rate,
                session_grid_generation.epoch(),
            )?;
            state.publish_root();
            state.reserved_session_grid = Some(session_grid_generation);
            state
                .ctx
                .as_mut()
                .ok_or(SessionError::NoContext)?
                .stop_stream();
            state.ctx = None;
            state.transport_control = None;
            state.mix_tap = None;
            state.transport = SessionTransportState::default();
            state.session_output_node_id = None;
            state.session_output_memo = None;
            state.session_limiter_node_id = None;
        }
        Ok(())
    }
    pub(super) fn remove_player_graph<B: AudioBackend, S>(
        fw_ctx: &mut FirewheelCtx<B>,
        player: &mut Deck<S>,
    ) {
        let player_id = player.player_id;
        for slot in player.slots.drain(..) {
            if let Err(err) = fw_ctx.remove_node(slot.volume_node_id) {
                warn!(player_id, ?err, "failed to remove slot volume node");
            }
            if let Err(err) = fw_ctx.remove_node(slot.player_node_id) {
                warn!(player_id, ?err, "failed to remove slot player node");
            }
        }
        if let Some(master_id) = player.master_volume_node_id.take()
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
    pub(super) fn clear_player_graph_state<S>(player: &mut Deck<S>) {
        player.master_eq_memo = None;
        player.master_volume_memo = None;
    }
}

pub(super) mod slots {
    use super::*;

    pub(in crate::session) fn allocate_slot<B, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
    ) -> Result<Reply, SessionError>
    where
        B: AudioBackend,
        S: HasPool<f32> + Send + Sync + 'static,
    {
        debug!(player_id, "[KITHARA-ROUTE] allocating player slot");
        let idx = player_index(state, player_id)?;
        if !deck_at(state, idx)?.started {
            return Err(SessionError::NotRunning(player_id));
        }
        let master_eq_id = deck_at(state, idx)?.master_eq_node_id;
        let (fw_ctx, master_eq_id) = match (&mut state.ctx, master_eq_id) {
            (None, _) => return Err(SessionError::NoContext),
            (Some(_), None) => return Err(graph_state("player master eq node is not initialised")),
            (Some(fw_ctx), Some(master_eq_id)) => (fw_ctx, master_eq_id),
        };
        let player = deck_at_mut(&mut state.graph, idx)?;
        let slot_id = SlotId::new(player.next_slot_id);
        player.next_slot_id += 1;
        let shared_eq = player.shared_eq.clone();
        let (inputs, control) = slot_channels(shared_eq);
        let player_node = PlayerNode::new(inputs, player.pools.clone()).with_session_context();
        let player_node_id = fw_ctx.add_node(player_node, None);
        let slot_volume = VolumeNode::from_linear(1.0);
        let slot_volume_memo = Memo::new(slot_volume);
        let slot_volume_id = fw_ctx.add_node(slot_volume, None);
        let player_to_slot = "connect player->slot_volume";
        connect_stereo(fw_ctx, player_node_id, slot_volume_id, player_to_slot)?;
        let slot_to_master = "connect slot_volume->player_master_eq";
        connect_stereo(fw_ctx, slot_volume_id, master_eq_id, slot_to_master)?;
        if let Err(err) = fw_ctx.update() {
            warn!(
                player_id,
                ?slot_id,
                "graph update after slot allocate failed: {err:?}"
            );
        }
        player.slots.push(SlotNodes {
            slot_id,
            player_node_id,
            volume_memo: slot_volume_memo,
            volume_node_id: slot_volume_id,
        });
        debug!(
            player_id,
            ?slot_id,
            ?player_node_id,
            ?slot_volume_id,
            slots = player.slots.len(),
            "[KITHARA-ROUTE] player slot allocated"
        );
        let reply = Reply::SlotAllocated(AllocatedSlot::new(control, slot_id));
        Ok(reply)
    }
    pub(in crate::session) fn release_slot<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
        slot: SlotId,
    ) -> Result<(), SessionError> {
        debug!(player_id, ?slot, "[KITHARA-ROUTE] releasing player slot");
        let idx = player_index(state, player_id)?;
        let slot_nodes = {
            let player = deck_at_mut(&mut state.graph, idx)?;
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
    pub(super) fn take_slot<S>(
        player: &mut Deck<S>,
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
        if let Err(err) = fw_ctx.remove_node(slot.volume_node_id) {
            warn!(player_id, ?err, "failed to remove slot volume node");
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

    /// Validates the whole request before mutating anything, so an invalid
    /// entry leaves the batch untouched. Omitted players are unchanged.
    pub(in crate::session) fn set_player_master_volumes<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        levels: &[PlayerLevel],
    ) -> Result<(), SessionError> {
        let mut resolved: Vec<(usize, f32)> = Vec::with_capacity(levels.len());
        for &PlayerLevel {
            player_id, level, ..
        } in levels
        {
            if !level.is_finite() || !(0.0..=1.0).contains(&level) {
                return Err(SessionError::MasterVolumeOutOfRange { player_id, level });
            }
            let idx = player_index(state, player_id)?;
            if resolved.iter().any(|&(seen, _)| seen == idx) {
                return Err(SessionError::DuplicatePlayer(player_id));
            }
            // Checked here so the apply pass below is infallible
            // (all-or-nothing).
            let player = deck_at(state, idx)?;
            if player.started
                && (state.ctx.is_none()
                    || player.master_volume_node_id.is_none()
                    || player.master_volume_memo.is_none())
            {
                return Err(graph_state("player master vol graph is not initialised"));
            }
            resolved.push((idx, level));
        }
        for &(idx, level) in &resolved {
            apply_master_volume(state, idx, level)?;
        }
        Ok(())
    }

    fn apply_master_volume<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        idx: usize,
        volume: f32,
    ) -> Result<(), SessionError> {
        let (ctx, graph) = (&mut state.ctx, &mut state.graph);
        let player = deck_at_mut(graph, idx)?;
        player.master_volume = volume;
        if let (Some(fw_ctx), Some(master_id), Some(memo)) = (
            ctx,
            player.master_volume_node_id,
            &mut player.master_volume_memo,
        ) {
            memo.volume = master_gain(volume);
            let mut queue = fw_ctx.event_queue(master_id);
            memo.update_memo(&mut queue);
        }
        Ok(())
    }
    pub(in crate::session) fn set_player_slot_volume<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
        slot: SlotId,
        volume: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !deck_at(state, idx)?.started {
            return Err(SessionError::NotRunning(player_id));
        }
        let (ctx, graph) = (&mut state.ctx, &mut state.graph);
        let Some(slot_nodes) = deck_at_mut(graph, idx)?
            .slots
            .iter_mut()
            .find(|s| s.slot_id == slot)
        else {
            return Err(SessionError::SlotNotFound(slot));
        };
        let fw_ctx = ctx.as_mut().ok_or(SessionError::NoContext)?;
        slot_nodes.volume_memo.volume = Volume::Linear(volume.clamp(0.0, 1.0));
        let mut queue = fw_ctx.event_queue(slot_nodes.volume_node_id);
        slot_nodes.volume_memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::session) fn set_player_eq_gain<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
        band: usize,
        gain_db: f32,
    ) -> Result<(), SessionError> {
        let idx = player_index(state, player_id)?;
        if !deck_at(state, idx)?.started {
            return Err(SessionError::NotRunning(player_id));
        }
        let (ctx, graph) = (&mut state.ctx, &mut state.graph);
        let player = deck_at_mut(graph, idx)?;
        let fw_ctx = ctx.as_mut().ok_or(SessionError::NoContext)?;
        let Some(master_eq_id) = player.master_eq_node_id else {
            return Err(graph_state("player master eq node is not initialised"));
        };
        let Some(memo) = &mut player.master_eq_memo else {
            return Err(graph_state("player master eq memo is not initialised"));
        };
        if band >= memo.band_count() {
            return Err(SessionError::EqBandOutOfRange {
                band,
                bands: memo.band_count(),
            });
        }
        memo.set_gain(band, GainDb::from(gain_db));
        let mut queue = fw_ctx.event_queue(master_eq_id);
        memo.update_memo(&mut queue);
        Ok(())
    }
    pub(in crate::session) fn set_player_eq_layout<B, S>(
        state: &mut SessionState<B, S>,
        player_id: PlayerId,
        eq_layout: Vec<EqBandConfig>,
    ) -> Result<(), SessionError>
    where
        B: AudioBackend,
        S: HasPool<f32> + Send + Sync + 'static,
    {
        let idx = player_index(state, player_id)?;
        let (eq_layout, gains) = prepare_eq_layout(eq_layout);
        if !deck_at(state, idx)?.started {
            let player = deck_at_mut(&mut state.graph, idx)?;
            player.eq_layout = eq_layout;
            player.shared_eq.replace(&gains);
            return Ok(());
        }

        let (old_eq_id, master_volume_id, slot_volume_ids, pools) = {
            let player = deck_at(state, idx)?;
            let old_eq_id = player
                .master_eq_node_id
                .ok_or_else(|| graph_state("player master eq node is not initialised"))?;
            let master_volume_id = player
                .master_volume_node_id
                .ok_or_else(|| graph_state("player master vol node is not initialised"))?;
            let slot_volume_ids = player
                .slots
                .iter()
                .map(|slot| slot.volume_node_id)
                .collect::<Vec<NodeID>>();
            (
                old_eq_id,
                master_volume_id,
                slot_volume_ids,
                player.pools.clone(),
            )
        };
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let eq_config = EqConfig::builder(pools).build();
        let master_eq = MasterEqNode::new(eq_config, &eq_layout);
        let master_eq_memo = Memo::new(master_eq.clone());
        let master_eq_id = fw_ctx.add_node(master_eq, None);

        let swap = slot_volume_ids
            .into_iter()
            .try_for_each(|slot_id| {
                connect_stereo(
                    fw_ctx,
                    slot_id,
                    master_eq_id,
                    "connect slot_volume->replacement master_eq",
                )
            })
            .and_then(|()| {
                connect_stereo(
                    fw_ctx,
                    master_eq_id,
                    master_volume_id,
                    "connect replacement master_eq->master_vol",
                )
            })
            .and_then(|()| {
                fw_ctx.remove_node(old_eq_id).map_err(|err| {
                    SessionError::Graph(format!("remove previous master_eq failed: {err}"))
                })
            });
        if let Err(err) = swap {
            if let Err(remove_err) = fw_ctx.remove_node(master_eq_id) {
                warn!(
                    player_id,
                    ?remove_err,
                    "failed to remove rejected replacement EQ node"
                );
            }
            return Err(err);
        }
        if let Err(err) = fw_ctx.update() {
            warn!(
                player_id,
                "graph update after EQ layout swap failed: {err:?}"
            );
        }

        let player = deck_at_mut(&mut state.graph, idx)?;
        player.eq_layout = eq_layout;
        player.shared_eq.replace(&gains);
        player.master_eq_node_id = Some(master_eq_id);
        player.master_eq_memo = Some(master_eq_memo);
        Ok(())
    }
    pub(in crate::session) fn set_session_ducking<B: AudioBackend, S>(
        state: &mut SessionState<B, S>,
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

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, num::NonZeroU32};

    use firewheel::{
        StreamInfo, backend::BackendProcessInfo, node::StreamStatus, processor::FirewheelProcessor,
    };
    use kithara_bufpool::testing::{TestPools, pools};
    use kithara_events::EventBus;
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;
    use kithara_warp::{
        Beat, BeatGrid, BeatGridQuery, BeatGridRevision, BeatGridState, BeatGridUnavailable,
        MapAxis, MapPoint, MapPosition, SessionAxis, SessionEpoch, SessionFrame,
    };

    use super::*;
    use crate::{
        api::{SessionTransportSnapshot, Tempo},
        effects::eq::generate_log_spaced_bands,
        session::{
            dispatch::{invalidate_audio_route, run_cmd},
            protocol::Cmd,
            testing::{attach_player, state as test_state},
        },
    };

    const BLOCK_FRAMES: usize = 128;

    /// The process-wide output device, held by whichever stream owns it.
    #[derive(Default)]
    struct AudioDevice {
        processor: Option<FirewheelProcessor<TestBackend>>,
        retired_processors: Vec<FirewheelProcessor<TestBackend>>,
        defer_processor_drop: bool,
        next_stream: u64,
        owner: u64,
    }

    thread_local! {
        static DEVICE: RefCell<AudioDevice> = RefCell::new(AudioDevice::default());
    }

    fn device<R>(f: impl FnOnce(&mut AudioDevice) -> R) -> R {
        DEVICE.with(|cell| f(&mut cell.borrow_mut()))
    }

    struct TestBackend {
        stream: u64,
    }

    type TestState = SessionState<TestBackend, TestPools>;

    impl Drop for TestBackend {
        fn drop(&mut self) {
            device(|dev| {
                if dev.owner == self.stream {
                    let processor = dev.processor.take();
                    if dev.defer_processor_drop {
                        dev.retired_processors.extend(processor);
                    }
                }
            });
        }
    }

    #[derive(Clone)]
    struct TestConfig {
        sample_rate: u32,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                sample_rate: TestState::DEFAULT_SAMPLE_RATE,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("test backend stream error")]
    struct TestBackendError;

    impl AudioBackend for TestBackend {
        type Config = TestConfig;
        type Enumerator = ();
        type Instant = Instant;
        type StartStreamError = TestBackendError;
        type StreamError = TestBackendError;

        fn delay_from_last_process(&self, _process_timestamp: Self::Instant) -> Option<Duration> {
            None
        }

        fn enumerator() -> Self::Enumerator {}

        fn poll_status(&mut self) -> Result<(), Self::StreamError> {
            Ok(())
        }

        fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
            device(|dev| {
                dev.owner = self.stream;
                dev.processor = Some(processor);
            });
        }

        fn start_stream(
            config: Self::Config,
        ) -> Result<(Self, StreamInfo), Self::StartStreamError> {
            let stream = device(|dev| {
                dev.next_stream += 1;
                dev.next_stream
            });
            let sample_rate = NonZeroU32::new(config.sample_rate).unwrap_or(
                NonZeroU32::new(TestState::DEFAULT_SAMPLE_RATE)
                    .expect("invariant: fixture default sample rate is non-zero"),
            );
            let max_block_frames = NonZeroU32::new(512).ok_or(TestBackendError)?;
            let stream_info = StreamInfo {
                sample_rate,
                sample_rate_recip: 1.0 / f64::from(sample_rate.get()),
                prev_sample_rate: sample_rate,
                max_block_frames,
                num_stream_in_channels: 0,
                num_stream_out_channels: 2,
                input_to_output_latency_seconds: 0.0,
                declick_frames: max_block_frames,
                output_device_id: String::from("test-output-device"),
                input_device_id: None,
            };
            Ok((Self { stream }, stream_info))
        }
    }

    fn start_test_stream(
        ctx: &mut FirewheelCtx<TestBackend>,
        sample_rate: u32,
    ) -> Result<(), String> {
        ctx.start_stream(TestConfig { sample_rate })
            .map_err(|err| err.to_string())
    }

    /// `false` means no stream owns the device, which is what silence looks like.
    fn deliver_one_block() -> bool {
        device(|dev| {
            let Some(processor) = dev.processor.as_mut() else {
                return false;
            };
            let mut output = [0.0_f32; BLOCK_FRAMES * 2];
            processor.process_interleaved(
                &[],
                &mut output,
                BackendProcessInfo {
                    num_in_channels: 0,
                    num_out_channels: 2,
                    frames: BLOCK_FRAMES,
                    process_timestamp: Instant::now(),
                    duration_since_stream_start: Duration::ZERO,
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status: StreamStatus::empty(),
                    dropped_frames: 0,
                },
            );
            true
        })
    }

    fn processed_frames(state: &TestState) -> i64 {
        state
            .ctx
            .as_ref()
            .map_or(-1, |fw_ctx| fw_ctx.audio_clock().samples.0)
    }

    fn register(state: &mut TestState) -> PlayerId {
        let grid_id = attach_player(state);
        match run_cmd(
            state,
            Cmd::RegisterPlayer {
                grid_id,
                bus: EventBus::default(),
                eq_layout: generate_log_spaced_bands(5),
                pools: pools(),
                sample_rate: TestState::DEFAULT_SAMPLE_RATE,
            },
        ) {
            Reply::PlayerRegistered(id) => id,
            Reply::Err(err) => panic!("player registration failed: {err}"),
            _ => panic!("player registration returned unexpected reply"),
        }
    }

    fn start_at(state: &mut TestState, player_id: PlayerId, sample_rate: u32) {
        match run_cmd(
            state,
            Cmd::StartPlayer {
                master_volume: 1.0,
                player_id,
                sample_rate,
                render_quantum_frames: NonZeroUsize::new(64)
                    .expect("fixture render quantum is non-zero"),
                response_budget_frames: NonZeroUsize::new(639)
                    .expect("fixture response budget is non-zero"),
            },
        ) {
            Reply::Ok => {}
            Reply::Err(err) => panic!("player {player_id} failed to start: {err}"),
            _ => panic!("player start returned unexpected reply"),
        }
    }

    fn start(state: &mut TestState, player_id: PlayerId) {
        start_at(state, player_id, TestState::DEFAULT_SAMPLE_RATE);
    }

    fn unregister(state: &mut TestState, player_id: PlayerId) {
        match run_cmd(state, Cmd::UnregisterPlayer { player_id }) {
            Reply::Ok => {}
            Reply::Err(err) => panic!("player {player_id} failed to unregister: {err}"),
            _ => panic!("player unregister returned unexpected reply"),
        }
    }

    fn set_tempo_and_read_session_grid(state: &mut TestState) -> SessionTransportSnapshot {
        assert!(matches!(
            run_cmd(
                state,
                Cmd::SetSessionTempo {
                    tempo: Tempo::new(120.0).expect("invariant: fixture tempo is valid"),
                },
            ),
            Reply::Ok
        ));
        assert!(deliver_one_block(), "transport commit must be rendered");
        match run_cmd(state, Cmd::QuerySessionTransport) {
            Reply::SessionTransport(snapshot) => snapshot,
            Reply::Err(error) => panic!("transport snapshot failed: {error}"),
            _ => panic!("transport query returned an unexpected reply"),
        }
    }

    #[kithara::test]
    fn a_running_player_replaces_its_eq_layout_without_releasing_slots() {
        device(|dev| *dev = AudioDevice::default());
        let mut state = test_state(start_test_stream);
        let player_id = register(&mut state);
        start(&mut state, player_id);
        let slot = match run_cmd(&mut state, Cmd::AllocateSlot { player_id }) {
            Reply::SlotAllocated(allocated) => allocated.slot,
            Reply::Err(err) => panic!("slot allocation failed: {err}"),
            _ => panic!("slot allocation returned unexpected reply"),
        };
        let previous_eq = deck_at(&state, 0)
            .expect("the registered deck is present")
            .master_eq_node_id;
        let previous_volume = deck_at(&state, 0)
            .expect("the registered deck is present")
            .master_volume_node_id;
        let mut layout = generate_log_spaced_bands(4);
        for (band, gain) in layout.iter_mut().zip([-6.0, -3.0, 1.5, 4.0]) {
            band.set_gain_db(GainDb::from(gain));
        }

        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::SetPlayerEqLayout {
                    player_id,
                    eq_layout: layout,
                },
            ),
            Reply::Ok
        ));

        let player = deck_at(&state, 0).expect("the registered deck is present");
        assert_eq!(player.eq_layout.len(), 4);
        assert_eq!(player.shared_eq.snapshot(), vec![-6.0, -3.0, 1.5, 4.0]);
        assert_eq!(player.slots.len(), 1);
        assert_eq!(player.slots[0].slot_id, slot);
        assert_ne!(player.master_eq_node_id, previous_eq);
        assert_eq!(player.master_volume_node_id, previous_volume);
        assert_eq!(
            player.master_eq_memo.as_ref().map(|memo| memo.band_count()),
            Some(4)
        );
        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::SetPlayerEqGain {
                    player_id,
                    band: 3,
                    gain_db: 5.0,
                },
            ),
            Reply::Ok
        ));
    }

    #[kithara::test]
    fn a_second_player_started_after_the_last_one_left_gets_a_processed_stream() {
        device(|dev| *dev = AudioDevice::default());
        let mut state = test_state(start_test_stream);

        let first = register(&mut state);
        start(&mut state, first);
        assert!(
            deliver_one_block(),
            "the first player's stream must own the output device"
        );

        unregister(&mut state, first);

        assert!(
            state.ctx.is_none(),
            "the session must release the output device once no player feeds it"
        );

        let second = register(&mut state);
        start(&mut state, second);
        assert!(matches!(
            run_cmd(&mut state, Cmd::AllocateSlot { player_id: second }),
            Reply::SlotAllocated(..)
        ));

        let before = processed_frames(&state);
        assert!(
            deliver_one_block(),
            "the second player's stream must own the output device"
        );
        assert!(
            processed_frames(&state) > before,
            "the second player's stream delivered no processed callback"
        );
    }

    #[kithara::test]
    fn idle_context_recreation_advances_generation_before_deferred_processor_drop() {
        device(|dev| {
            *dev = AudioDevice::default();
            dev.defer_processor_drop = true;
        });
        let mut state = test_state(start_test_stream);
        let first_player = register(&mut state);
        let initial = state.root.snapshot();
        assert_eq!(initial.revision(), BeatGridRevision::first());
        assert_eq!(
            initial.state(),
            BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
        );
        assert_eq!(
            initial.axis(),
            MapAxis::Session(SessionAxis::new(
                NonZeroU32::new(TestState::DEFAULT_SAMPLE_RATE)
                    .expect("the fixture sample rate is non-zero"),
                SessionEpoch::new(0),
            ))
        );
        assert_eq!(
            state
                .reserved_session_grid
                .expect("registration seeds session-grid generation")
                .stamp()
                .expect("the initial session-grid revision is committed"),
            initial.stamp()
        );
        start_at(&mut state, first_player, 0);
        let before = set_tempo_and_read_session_grid(&mut state);
        let first_live = state.root.snapshot();
        assert_eq!(first_live, before.session_grid());
        assert_eq!(
            first_live.revision(),
            initial
                .revision()
                .checked_next()
                .expect("the fixture grid revision can advance")
        );
        let old_beat = MapPoint::new(
            before.session_grid_stamp(),
            Beat::new(1.0).expect("invariant: fixture beat is finite"),
        );

        assert!(matches!(
            invalidate_audio_route(&mut state, "deferred route before idle teardown"),
            Reply::Ok
        ));
        let route_boundary = state.root.snapshot();
        assert_eq!(
            state
                .reserved_session_grid
                .expect("the deferred route owns a reserved generation")
                .stamp()
                .expect("the deferred route reservation has a revision"),
            route_boundary.stamp()
        );
        assert!(state.stream_needs_restart);

        unregister(&mut state, first_player);
        let unavailable = state.root.snapshot();
        assert_eq!(
            unavailable.revision(),
            first_live
                .revision()
                .checked_next()
                .and_then(BeatGridRevision::checked_next)
                .expect("the fixture grid revision can advance twice")
        );
        assert_eq!(
            unavailable.state(),
            BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
        );
        assert_eq!(
            unavailable.axis(),
            MapAxis::Session(SessionAxis::new(
                NonZeroU32::new(TestState::DEFAULT_SAMPLE_RATE)
                    .expect("the fixture sample rate is non-zero"),
                SessionEpoch::new(2),
            ))
        );
        assert_eq!(
            state
                .reserved_session_grid
                .expect("idle teardown returns session-grid generation")
                .stamp()
                .expect("the restart boundary has a reserved revision"),
            unavailable.stamp()
        );
        assert!(
            state.ctx.is_none(),
            "idle teardown must destroy the context"
        );
        device(|dev| {
            assert_eq!(
                dev.retired_processors.len(),
                1,
                "old processor must still be alive while the new context starts"
            );
        });

        let second_player = register(&mut state);
        start_at(&mut state, second_player, 0);
        let after = set_tempo_and_read_session_grid(&mut state);
        let second_live = state.root.snapshot();
        assert_eq!(second_live, after.session_grid());
        assert_eq!(
            second_live.revision(),
            unavailable
                .revision()
                .checked_next()
                .expect("the fixture grid revision can advance")
        );

        assert_eq!(
            before.session_grid_stamp().grid_id(),
            after.session_grid_stamp().grid_id(),
            "one session keeps one session-grid identity"
        );
        assert!(after.session_epoch() > before.session_epoch());
        assert!(after.session_grid_stamp().revision() > before.session_grid_stamp().revision());
        assert!(matches!(
            after.session_grid().position_at(old_beat),
            BeatGridQuery::Stale { expected, given }
                if expected == after.session_grid_stamp()
                    && given == before.session_grid_stamp()
        ));
        device(|dev| {
            assert_eq!(dev.retired_processors.len(), 1);
            dev.retired_processors.clear();
            dev.defer_processor_drop = false;
        });
    }

    #[kithara::test]
    fn deferred_route_restart_converges_before_unrendered_idle_shutdown() {
        device(|dev| {
            *dev = AudioDevice::default();
            dev.defer_processor_drop = true;
        });
        let mut state = test_state(start_test_stream);
        let player = register(&mut state);
        start_at(&mut state, player, 0);
        let live = set_tempo_and_read_session_grid(&mut state);

        assert!(matches!(
            invalidate_audio_route(&mut state, "test route restart"),
            Reply::Ok
        ));
        let reserved = state.root.snapshot();
        assert!(reserved.revision() > live.session_grid_stamp().revision());
        assert_eq!(
            state
                .reserved_session_grid
                .expect("the delayed processor keeps an exact route reservation")
                .stamp()
                .expect("the route reservation has a revision"),
            reserved.stamp()
        );
        assert!(state.stream_needs_restart);
        let Reply::SampleRate(rate) = run_cmd(&mut state, Cmd::QuerySampleRate) else {
            panic!("the pending route restart must answer the sample-rate query")
        };
        assert_eq!(rate.measured, None);
        assert_eq!(rate.requested, 0);
        assert!(matches!(
            run_cmd(&mut state, Cmd::QuerySessionTransport),
            Reply::Err(SessionError::TransportNotProcessed)
        ));
        assert_eq!(
            state.root.snapshot(),
            reserved,
            "a stale transport observation must not replace the route reservation"
        );
        assert!(matches!(
            run_cmd(
                &mut state,
                Cmd::SetSessionTempo {
                    tempo: Tempo::new(121.0).expect("invariant: fixture tempo is valid"),
                },
            ),
            Reply::Err(SessionError::TransportNotProcessed)
        ));
        assert_eq!(
            state.root.snapshot(),
            reserved,
            "a transport command must not cross an unfinished route boundary"
        );
        device(|dev| {
            assert_eq!(dev.retired_processors.len(), 1);
            dev.retired_processors.clear();
            dev.defer_processor_drop = false;
        });
        assert!(matches!(run_cmd(&mut state, Cmd::Tick), Reply::Ok));
        assert!(!state.stream_needs_restart);
        assert!(state.reserved_session_grid.is_none());
        let converged = state
            .transport_control
            .as_mut()
            .expect("the restarted stream keeps transport control")
            .observation()
            .session_grid()
            .stamp()
            .expect("the restarted transport has a grid revision");
        assert_eq!(converged, reserved.stamp());
        let restart_frame = SessionFrame::new(
            state
                .ctx
                .as_ref()
                .expect("the restarted stream keeps its context")
                .audio_clock()
                .samples
                .0,
        );

        assert!(
            deliver_one_block(),
            "the restarted processor must render its preserved transport"
        );
        let restarted = match run_cmd(&mut state, Cmd::QuerySessionTransport) {
            Reply::SessionTransport(snapshot) => snapshot,
            Reply::Err(error) => panic!("restarted transport snapshot failed: {error}"),
            _ => panic!("restarted transport query returned an unexpected reply"),
        };
        let published = state.root.snapshot();
        assert_eq!(published.state(), BeatGridState::Live);
        let MapAxis::Session(reserved_axis) = reserved.axis() else {
            panic!("the route reservation uses the session axis")
        };
        let MapAxis::Session(published_axis) = published.axis() else {
            panic!("the restarted grid uses the session axis")
        };
        assert_eq!(published_axis.epoch(), reserved_axis.epoch());
        assert!(published.revision() > reserved.revision());
        assert_eq!(published, restarted.session_grid());
        assert_eq!(
            restarted
                .anchor()
                .frame_at(live.position())
                .expect("the preserved beat is representable on the restarted axis"),
            restart_frame
        );
        let old_position = MapPoint::new(
            live.session_grid_stamp(),
            MapPosition::Session(SessionFrame::new(0)),
        );
        assert!(matches!(
            published.beat_at(old_position),
            BeatGridQuery::Stale { .. }
        ));

        unregister(&mut state, player);

        let unavailable = state.root.snapshot();
        assert!(unavailable.revision() > reserved.revision());
        assert_eq!(
            unavailable.state(),
            BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
        );
        assert!(state.ctx.is_none());
    }
}
