use firewheel::{FirewheelContext, node::NodeID};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_events::EventBus;
use kithara_output::OutputGroup;
use kithara_render::bridge::MixerInputs;
use kithara_warp::MapAxis;
use tracing::{debug, warn};

use super::{
    SessionError,
    queue::settle_root_receipts,
    state::{DeckNode, SessionState, TapSlot, add_graph_node, ensure_ctx},
    transport::TransportState,
};
use crate::{
    DeckId,
    rt::{PlayerNode, TapNode},
};

pub(crate) fn install_deck<T, S>(
    state: &mut SessionState<T, S>,
    id: DeckId,
    inputs: MixerInputs,
    pools: PoolRegion<S>,
    bus: Option<EventBus>,
) -> Result<(), SessionError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    if state.deck_nodes.iter().any(|deck| deck.id == id) {
        return Err(SessionError::DeckAttached(id));
    }
    ensure_ctx(state)?;
    let master = state
        .session_output_node_id
        .ok_or(SessionError::NoContext)?;
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    let node = add_graph_node(ctx, PlayerNode::<S, TransportState>::new(inputs, pools))?;
    let installed =
        connect_stereo(ctx, node, master, "connect deck mixer to master").and_then(|()| {
            ctx.update()
                .map_err(|error| SessionError::Graph(format!("{error:?}")))
        });
    if let Err(error) = installed {
        if let Err(remove_error) = ctx.remove_node(node) {
            warn!(?remove_error, "failed to remove the rejected deck node");
        }
        return Err(error);
    }
    state.deck_nodes.push(DeckNode { id, node, bus });
    Ok(())
}

/// Removes a deck node after its command scope reports Closed.
pub(crate) fn remove_deck<T, S>(
    state: &mut SessionState<T, S>,
    id: DeckId,
) -> Result<(), SessionError> {
    let index = state
        .deck_nodes
        .iter()
        .position(|deck| deck.id == id)
        .ok_or(SessionError::DeckNotFound(id))?;
    let node = state.deck_nodes[index].node;
    let ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
    ctx.remove_node(node)
        .map_err(|error| SessionError::Graph(format!("remove deck mixer failed: {error}")))?;
    state.deck_nodes.remove(index);
    if let Err(error) = ctx.update() {
        warn!(?error, "graph update after deck retirement failed");
    }
    Ok(())
}

/// Stops the stream while retaining its context, scopes and applied settings.
pub(crate) fn idle<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if state.ctx.is_none() {
        return Ok(());
    }
    let observed = state
        .transport_observation
        .as_mut()
        .ok_or_else(|| SessionError::Graph("session transport observation is missing".to_owned()))?
        .read()
        .session_grid();
    let mut generation = state
        .reserved_session_grid
        .map_or(Ok(observed), |reserved| reserved.promote(observed))
        .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
    generation
        .advance_restart()
        .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
    let stamp = generation
        .stamp()
        .map_err(|error| SessionError::Graph(error.message().to_owned()))?;
    let MapAxis::Session(axis) = state.root.grid().axis() else {
        return Err(SessionError::Graph(
            "session host published a non-session grid axis".to_owned(),
        ));
    };
    state
        .root
        .publish_unavailable(stamp, axis.sample_rate(), generation.epoch());
    state.reserved_session_grid = Some(generation);
    state.iteration_clock = None;
    state
        .ctx
        .as_mut()
        .ok_or(SessionError::NoContext)?
        .request_deactivate();
    state.stream = None;
    state.stream_needs_restart = false;
    state.publish_root();
    Ok(())
}

/// Drops an empty, stopped context after every scope is Closed and its owner
/// has settled the root receipts. Only a retained output keeps the context alive.
pub(crate) fn drop_idle_context<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if !state.deck_nodes.is_empty() || state.stream.is_some() {
        return Err(SessionError::Graph(
            "cannot drop a context with deck nodes or a running stream".to_owned(),
        ));
    }
    if state.retains_output || state.ctx.is_none() {
        return Ok(());
    }
    settle_root_receipts(state);
    state.settings.abandon();
    state.ctx.take();
    state.channel = None;
    state.transport_observation = None;
    state.session_output_node_id = None;
    state.session_limiter_node_id = None;
    state.session_metronome_node_id = None;
    for tap in [crate::api::Tap::Master, crate::api::Tap::Output] {
        let slot = state.taps.slot(tap);
        if matches!(slot, Some(TapSlot::Installed(_))) {
            *slot = None;
        }
    }
    state.publish_root();
    Ok(())
}

fn connect_stereo(
    fw_ctx: &mut FirewheelContext,
    from: NodeID,
    to: NodeID,
    label: &'static str,
) -> Result<(), SessionError> {
    fw_ctx
        .connect(from, to, &[(0, 0), (1, 1)], false)
        .map(|_| ())
        .map_err(|err| SessionError::Graph(format!("{label} failed: {err}")))
}

pub(crate) mod tap {
    use super::*;
    use crate::api::Tap;

    pub(crate) fn attach<T, S>(
        state: &mut SessionState<T, S>,
        tap: Tap,
        outputs: OutputGroup,
    ) -> Result<(), SessionError> {
        if state.taps.slot(tap).is_some() {
            return Err(SessionError::TapActive);
        }
        let Some(from) = source(state, tap) else {
            *state.taps.slot(tap) = Some(TapSlot::Requested(outputs));
            return Ok(());
        };
        install(state, tap, from, outputs)
    }

    pub(crate) fn detach<T, S>(state: &mut SessionState<T, S>, tap: Tap) {
        let Some(TapSlot::Installed(tap_id)) = state.taps.slot(tap).take() else {
            return;
        };
        let Some(ref mut fw_ctx) = state.ctx else {
            return;
        };
        if let Err(err) = fw_ctx.remove_node(tap_id) {
            warn!(?tap, ?err, "failed to remove session tap node");
        }
        if let Err(err) = fw_ctx.update() {
            warn!(?tap, "graph update after tap detach failed: {err:?}");
        }
    }

    pub(crate) fn install_requested<T, S>(
        state: &mut SessionState<T, S>,
    ) -> Result<(), SessionError> {
        for tap in [Tap::Master, Tap::Output] {
            let Some(from) = source(state, tap) else {
                continue;
            };
            let slot = state.taps.slot(tap);
            if !matches!(slot, Some(TapSlot::Requested(_))) {
                continue;
            }
            let Some(TapSlot::Requested(outputs)) = slot.take() else {
                continue;
            };
            install(state, tap, from, outputs)?;
        }
        Ok(())
    }

    fn source<T, S>(state: &SessionState<T, S>, tap: Tap) -> Option<NodeID> {
        match tap {
            Tap::Master => state.session_limiter_node_id,
            Tap::Output => state.session_metronome_node_id,
        }
    }

    fn install<T, S>(
        state: &mut SessionState<T, S>,
        tap: Tap,
        from: NodeID,
        outputs: OutputGroup,
    ) -> Result<(), SessionError> {
        let fw_ctx = state.ctx.as_mut().ok_or(SessionError::NoContext)?;
        let tap_id = add_graph_node(fw_ctx, TapNode::new(outputs))?;
        if let Err(err) = connect_stereo(fw_ctx, from, tap_id, "connect session output->tap") {
            if let Err(remove_err) = fw_ctx.remove_node(tap_id) {
                warn!(?remove_err, "failed to remove the unconnected tap node");
            }
            return Err(err);
        }
        if let Err(err) = fw_ctx.update() {
            warn!("graph update after tap install failed: {err:?}");
        }
        *state.taps.slot(tap) = Some(TapSlot::Installed(tap_id));
        debug!(?tap, ?tap_id, "[KITHARA-ROUTE] session tap installed");
        Ok(())
    }
}
