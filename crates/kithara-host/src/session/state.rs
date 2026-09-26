use std::num::NonZeroU32;

use arc_swap::ArcSwap;
use firewheel::{
    FirewheelConfig, FirewheelContext,
    channel_config::ChannelCount,
    diff::Memo,
    node::{AudioNode, NodeID},
    nodes::volume::VolumeNode,
    param::smoother::SmootherConfig,
};
use kithara_bufpool::PoolRegion;
use kithara_effects::{GainDb, LimiterConfig, eq::EqBandConfig};
use kithara_events::EventBus;
use kithara_output::OutputGroup;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_play::{SessionSampleRate, StreamShape, session::RegisteredPlayer};
use kithara_sync::{GroupState, SyncError, SyncGroup, SyncGroupSnapshot, SyncStatusSnapshot};
use kithara_warp::{BeatGrid, BeatGridId, BeatGridRevision, BeatGridSnapshot};
use tracing::{debug, warn};

use super::{
    dispatch::{restart_stream, sample_rate, stream_shape, trace_stream_info},
    graph::tap,
    protocol::{PlayerId, SessionError, StartStreamFn},
    transport::{SessionGridGeneration, SessionTransportState, TransportControl, install},
};
use crate::{
    PlayerMember,
    api::{SessionDuckingMode, SlotId},
    bridge::SharedEq,
    rt::{LimiterNode, MasterEqNode},
};

#[derive(Debug)]
pub(super) struct SlotNodes {
    pub(super) volume_memo: Memo<VolumeNode>,
    pub(super) player_node_id: NodeID,
    pub(super) volume_node_id: NodeID,
    pub(super) slot_id: SlotId,
}

pub(super) struct Deck<S> {
    pub(super) grid_id: BeatGridId,
    pub(super) bus: EventBus,
    pub(super) master_eq_memo: Option<Memo<MasterEqNode<S>>>,
    pub(super) master_eq_node_id: Option<NodeID>,
    pub(super) master_volume_memo: Option<Memo<VolumeNode>>,
    pub(super) master_volume_node_id: Option<NodeID>,
    pub(super) player_id: PlayerId,
    pub(super) pools: PoolRegion<S>,
    pub(super) shared_eq: SharedEq,
    pub(super) gate_smoothing: SmootherConfig,
    pub(super) eq_layout: Vec<EqBandConfig>,
    pub(super) slots: Vec<SlotNodes>,
    pub(super) started: bool,
    pub(super) master_volume: f32,
    pub(super) next_slot_id: u64,
}

impl<S> Deck<S> {
    pub(super) fn new(
        player_id: PlayerId,
        grid_id: BeatGridId,
        bus: EventBus,
        eq_layout: Vec<EqBandConfig>,
        pools: PoolRegion<S>,
        master_volume: f32,
        gate_smoothing: SmootherConfig,
    ) -> Self {
        let (eq_layout, gains) = prepare_eq_layout(eq_layout);
        let band_count = eq_layout.len();
        let shared_eq = SharedEq::new(band_count);
        shared_eq.replace(&gains);
        Self {
            bus,
            eq_layout,
            gate_smoothing,
            pools,
            player_id,
            grid_id,
            master_volume,
            shared_eq,
            master_eq_memo: None,
            master_eq_node_id: None,
            master_volume_memo: None,
            master_volume_node_id: None,
            next_slot_id: 1,
            slots: Vec::new(),
            started: false,
        }
    }
}

#[derive_where::derive_where(Default)]
pub(super) struct GraphRegistry<S> {
    decks: Vec<Deck<S>>,
}

impl<S> GraphRegistry<S> {
    pub(super) fn index_by_grid(&self, grid_id: BeatGridId) -> Option<usize> {
        self.decks
            .iter()
            .position(|candidate| candidate.grid_id == grid_id)
    }

    pub(super) fn index_by_player(&self, player_id: PlayerId) -> Option<usize> {
        self.decks
            .iter()
            .position(|candidate| candidate.player_id == player_id)
    }

    pub(super) fn insert(&mut self, deck: Deck<S>) -> Result<(), SessionError> {
        if self
            .decks
            .iter()
            .any(|candidate| candidate.grid_id == deck.grid_id)
        {
            return Err(SessionError::Graph(
                "player grid is already projected into the session graph".to_owned(),
            ));
        }
        self.decks.push(deck);
        Ok(())
    }

    pub(super) fn remove(&mut self, index: usize) -> Option<Deck<S>> {
        (index < self.decks.len()).then(|| self.decks.remove(index))
    }

    delegate::delegate! {
        to self.decks {
            #[call(get)]
            pub(super) fn deck(&self, index: usize) -> Option<&Deck<S>>;
            #[call(get_mut)]
            pub(super) fn deck_mut(&mut self, index: usize) -> Option<&mut Deck<S>>;
            #[call(iter)]
            pub(super) fn decks(&self) -> impl Iterator<Item = &Deck<S>>;
            pub(super) fn len(&self) -> usize;
        }
    }
}

pub(super) fn prepare_eq_layout(eq_layout: Vec<EqBandConfig>) -> (Vec<EqBandConfig>, Vec<GainDb>) {
    let gains = eq_layout.iter().map(EqBandConfig::gain_db).collect();
    (eq_layout, gains)
}

pub(super) enum MixTap {
    Requested(OutputGroup),
    Installed(NodeID),
}

struct RootSnapshot {
    grid: BeatGridSnapshot,
    stream_shape: Option<StreamShape>,
    topology: Result<SyncGroupSnapshot, SyncError>,
    sample_rate: SessionSampleRate,
    status: SyncStatusSnapshot,
}

#[derive(Clone)]
pub(crate) struct RootView(Arc<ArcSwap<RootSnapshot>>);

impl RootView {
    pub(crate) fn new(root: &GroupState<PlayerMember>, sample_rate: NonZeroU32) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(RootSnapshot {
            grid: root.snapshot(),
            stream_shape: None,
            sample_rate: SessionSampleRate::new(None, sample_rate.get()),
            status: root.status(),
            topology: root.topology(),
        })))
    }

    fn publish(
        &self,
        root: &GroupState<PlayerMember>,
        stream_shape: Option<StreamShape>,
        sample_rate: SessionSampleRate,
    ) {
        self.0.store(Arc::new(RootSnapshot {
            stream_shape,
            sample_rate,
            grid: root.snapshot(),
            status: root.status(),
            topology: root.topology(),
        }));
    }

    delegate::delegate! {
        to self.0 {
            #[call(load)]
            #[expr($.grid.clone())]
            pub(crate) fn grid(&self) -> BeatGridSnapshot;
            #[call(load)]
            #[expr($.sample_rate)]
            pub(crate) fn sample_rate(&self) -> SessionSampleRate;
            #[call(load)]
            #[expr($.stream_shape)]
            pub(crate) fn stream_shape(&self) -> Option<StreamShape>;
            #[call(load)]
            #[expr($.status)]
            pub(crate) fn status(&self) -> SyncStatusSnapshot;
            #[call(load)]
            #[expr($.topology.clone())]
            pub(crate) fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
        }
    }
}

pub(crate) struct SessionState<T, S> {
    pub(super) graph: GraphRegistry<S>,
    pub(super) root: GroupState<PlayerMember>,
    pub(super) limiter: LimiterConfig,
    pub(super) ctx: Option<FirewheelContext>,
    pub(super) mix_tap: Option<MixTap>,
    /// The pause/resume fade length the session asks Firewheel for, in frames.
    /// `None` leaves Firewheel's own default in place.
    pub(super) requested_declick_frames: Option<NonZeroU32>,
    pub(super) requested_max_block_frames: Option<NonZeroU32>,
    pub(super) reserved_session_grid: Option<SessionGridGeneration>,
    pub(super) session_limiter_node_id: Option<NodeID>,
    pub(super) session_output_memo: Option<Memo<VolumeNode>>,
    pub(super) session_output_node_id: Option<NodeID>,
    pub(super) stream: Option<T>,
    pub(super) transport_control: Option<TransportControl>,
    pub(super) next_player_id: PlayerId,
    pub(super) root_view: RootView,
    pub(super) session_ducking: SessionDuckingMode,
    pub(super) transport: SessionTransportState,
    pub(super) start_stream_fn: StartStreamFn<T>,
    /// Set when the output device is acquired once and cannot be rebuilt, so
    /// an idle session must keep it rather than release it.
    pub(super) retains_output: bool,
    pub(super) stream_needs_restart: bool,
    pub(super) sample_rate_hint: u32,
}

/// The stream outlives nothing: it is dropped before the context.
///
/// Firewheel hands the stream its processor and waits, on its own drop, for
/// that processor to come back. Declaration order would drop the context
/// first, leaving it to wait out its whole deactivation timeout for a
/// processor this state still owns.
impl<T, S> Drop for SessionState<T, S> {
    fn drop(&mut self) {
        self.stream.take();
        self.ctx.take();
    }
}

impl<T, S> SessionState<T, S> {
    #[cfg(test)]
    pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 44_100;

    /// Creates session state with its own musical-grid topology.
    #[must_use]
    pub(crate) fn new<F>(
        root: GroupState<PlayerMember>,
        root_view: RootView,
        sample_rate: NonZeroU32,
        requested_max_block_frames: Option<NonZeroU32>,
        requested_declick_frames: Option<NonZeroU32>,
        limiter: LimiterConfig,
        start_stream_fn: F,
    ) -> Self
    where
        F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
    {
        let grid_id = root.id();
        let mut generation = SessionGridGeneration::new(grid_id);
        generation.commit_revision(BeatGridRevision::first());
        let state = Self {
            requested_max_block_frames,
            requested_declick_frames,
            limiter,
            root,
            root_view,
            start_stream_fn: Box::new(start_stream_fn),
            ctx: None,
            stream: None,
            transport_control: None,
            mix_tap: None,
            next_player_id: 1,
            sample_rate_hint: sample_rate.get(),
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
            session_limiter_node_id: None,
            retains_output: false,
            stream_needs_restart: false,
            transport: SessionTransportState::default(),
            reserved_session_grid: Some(generation),
            graph: GraphRegistry::default(),
        };
        state.publish_root();
        state
    }

    pub(super) fn publish_root(&self) {
        self.root_view
            .publish(&self.root, stream_shape(self), sample_rate(self));
    }
}

/// Adds a node to the graph, turning the rejection Firewheel now reports into
/// the session's own graph error. A node the graph refuses is a wiring bug, not
/// a runtime condition the session can route around.
pub(super) fn add_graph_node<N: AudioNode + 'static>(
    ctx: &mut FirewheelContext,
    node: N,
) -> Result<NodeID, SessionError> {
    ctx.add_node(node, None)
        .map_err(|err| SessionError::Graph(format!("audio graph rejected a node: {err}")))
}

pub(super) fn register_player<T, S>(
    state: &mut SessionState<T, S>,
    grid_id: BeatGridId,
    bus: EventBus,
    eq_layout: Vec<EqBandConfig>,
    pools: PoolRegion<S>,
    sample_rate: u32,
    gate_smoothing: SmootherConfig,
) -> Result<RegisteredPlayer, SessionError> {
    NonZeroU32::new(sample_rate).ok_or(SessionError::InvalidSampleRate(sample_rate))?;
    let player_id = state.next_player_id;
    let next_player_id = player_id
        .checked_add(1)
        .ok_or(SessionError::PlayerIdExhausted)?;
    let master_volume = state
        .root
        .with_group(grid_id, PlayerMember::host_level)
        .ok_or_else(|| {
            SessionError::Graph(
                "player must be attached to the host before graph registration".to_owned(),
            )
        })?;
    if !master_volume.is_finite() || !(0.0..=1.0).contains(&master_volume) {
        return Err(SessionError::MasterVolumeOutOfRange {
            player_id,
            level: master_volume,
        });
    }
    let deck = Deck::new(
        player_id,
        grid_id,
        bus,
        eq_layout,
        pools,
        master_volume,
        gate_smoothing,
    );
    let registration = RegisteredPlayer {
        id: player_id,
        eq: deck.shared_eq.clone(),
    };
    state.graph.insert(deck)?;
    state.next_player_id = next_player_id;
    debug!(
        player_id,
        players = state.graph.len(),
        "[KITHARA-ROUTE] session player registered"
    );
    Ok(registration)
}

pub(super) fn ensure_ctx<T, S>(
    state: &mut SessionState<T, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    ensure_stream_ready(state, sample_rate)?;
    ensure_session_output(state)
}

fn ensure_stream_ready<T, S>(
    state: &mut SessionState<T, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    if state.ctx.is_none() {
        return create_firewheel_context(state, sample_rate);
    }

    if state.stream_needs_restart {
        debug!(
            sample_rate,
            "[KITHARA-ROUTE] ensuring stopped stream is restarted"
        );
        restart_stream(state, sample_rate)?;
    }

    Ok(())
}

/// Converts the fade through `Duration` rather than casting directly, since Firewheel takes the
/// fade in seconds while the frame count is the session's own unit.
fn create_firewheel_context<T, S>(
    state: &mut SessionState<T, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    debug!(sample_rate, "[KITHARA-ROUTE] creating firewheel context");
    let mut config = FirewheelConfig {
        num_graph_outputs: ChannelCount::STEREO,
        ..FirewheelConfig::default()
    };
    if let Some(declick_frames) = state.requested_declick_frames {
        config.declick_seconds =
            Duration::from_secs_f64(f64::from(declick_frames.get()) / f64::from(sample_rate))
                .as_secs_f32();
    }
    let mut ctx = FirewheelContext::new(config);
    let session_grid = state
        .reserved_session_grid
        .take()
        .ok_or_else(|| SessionError::Graph("session grid generation is missing".to_owned()))?;
    let transport_control = match install(&mut ctx, session_grid) {
        Ok(control) => control,
        Err(error) => {
            state.reserved_session_grid = Some(session_grid);
            return Err(SessionError::Graph(error.into()));
        }
    };
    let stream = match (state.start_stream_fn)(&mut ctx, sample_rate) {
        Ok(stream) => stream,
        Err(error) => {
            state.reserved_session_grid = Some(session_grid);
            return Err(SessionError::StreamStart(error));
        }
    };
    state.ctx = Some(ctx);
    state.stream = Some(stream);
    state.transport_control = Some(transport_control);
    state.sample_rate_hint = sample_rate;
    state.stream_needs_restart = false;
    state.publish_root();
    trace_stream_info(state, "start-stream");
    debug!(sample_rate, "[KITHARA-ROUTE] firewheel context ready");
    Ok(())
}

fn ensure_session_output<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if state.session_output_node_id.is_none() {
        return create_session_output(state);
    }

    Ok(())
}

fn create_session_output<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    debug!("[KITHARA-ROUTE] creating session output graph");
    let limiter = LimiterNode::new(state.limiter);
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err(SessionError::NoContext);
    };
    let session_node = VolumeNode::from_linear(state.session_ducking.gain());
    let session_memo = Memo::new(session_node);
    let session_id = add_graph_node(fw_ctx, session_node)?;
    let limiter_id = add_graph_node(fw_ctx, limiter)?;
    let graph_out = fw_ctx.graph_out_node_id();
    fw_ctx
        .connect(session_id, limiter_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect session output to limiter failed: {err}"))
        })?;
    fw_ctx
        .connect(limiter_id, graph_out, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect limiter to graph_out failed: {err}"))
        })?;
    if let Err(err) = fw_ctx.update() {
        warn!("session graph update after output init failed: {err:?}");
    }
    state.session_output_node_id = Some(session_id);
    state.session_output_memo = Some(session_memo);
    state.session_limiter_node_id = Some(limiter_id);
    tap::install_requested(state, limiter_id)?;
    debug!(
        ?session_id,
        ?limiter_id,
        "[KITHARA-ROUTE] session output graph ready"
    );
    Ok(())
}
