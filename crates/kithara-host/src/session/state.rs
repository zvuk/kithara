use std::num::NonZeroU32;

use arc_swap::ArcSwap;
use firewheel::{
    FirewheelConfig, FirewheelCtx, backend::AudioBackend, channel_config::ChannelCount, diff::Memo,
    node::NodeID, nodes::volume::VolumeNode,
};
use kithara_bufpool::PoolRegion;
use kithara_events::EventBus;
use kithara_platform::sync::Arc;
use kithara_play::{GroupState, StreamShape, player::PlayerMember};
use kithara_warp::{
    BeatGrid, BeatGridId, BeatGridRevision, SyncError, SyncGroup, SyncGroupSnapshot,
    SyncStatusSnapshot,
};
use tracing::{debug, warn};

use super::{
    dispatch::{restart_stream, trace_stream_info},
    graph::{ducking_gain, tap},
    protocol::{PlayerId, SessionError, StartStreamFn},
    transport::{SessionGridGeneration, SessionTransportState, TransportControl, install},
};
use crate::{
    api::{SessionDuckingMode, SlotId},
    bridge::{MixTapWriter, SharedEq},
    effects::eq::{EqBandConfig, GainDb},
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
    pub(super) bus: EventBus,
    pub(super) master_eq_memo: Option<Memo<MasterEqNode<S>>>,
    pub(super) master_eq_node_id: Option<NodeID>,
    pub(super) master_volume_memo: Option<Memo<VolumeNode>>,
    pub(super) master_volume_node_id: Option<NodeID>,
    pub(super) pools: PoolRegion<S>,
    pub(super) player_id: PlayerId,
    pub(super) grid_id: BeatGridId,
    pub(super) shared_eq: SharedEq,
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
    ) -> Self {
        let (eq_layout, gains) = prepare_eq_layout(eq_layout);
        let band_count = eq_layout.len();
        let shared_eq = SharedEq::new(band_count);
        shared_eq.replace(&gains);
        Self {
            bus,
            eq_layout,
            pools,
            player_id,
            grid_id,
            master_eq_memo: None,
            master_eq_node_id: None,
            master_volume,
            master_volume_memo: None,
            master_volume_node_id: None,
            next_slot_id: 1,
            shared_eq,
            slots: Vec::new(),
            started: false,
        }
    }
}

pub(super) struct GraphRegistry<S> {
    decks: Vec<Deck<S>>,
}

impl<S> Default for GraphRegistry<S> {
    fn default() -> Self {
        Self { decks: Vec::new() }
    }
}

impl<S> GraphRegistry<S> {
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
}

pub(super) fn prepare_eq_layout(eq_layout: Vec<EqBandConfig>) -> (Vec<EqBandConfig>, Vec<GainDb>) {
    let gains = eq_layout.iter().map(EqBandConfig::gain_db).collect();
    (eq_layout, gains)
}

pub(super) enum MixTap {
    Requested(MixTapWriter),
    Installed(NodeID),
}

struct RootSnapshot {
    grid: kithara_warp::BeatGridSnapshot,
    status: SyncStatusSnapshot,
    topology: Result<SyncGroupSnapshot, SyncError>,
}

#[derive(Clone)]
pub(crate) struct RootView(Arc<ArcSwap<RootSnapshot>>);

impl RootView {
    pub(crate) fn new(root: &GroupState<PlayerMember>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(RootSnapshot {
            grid: root.snapshot(),
            status: root.status(),
            topology: root.topology(),
        })))
    }

    pub(crate) fn grid(&self) -> kithara_warp::BeatGridSnapshot {
        self.0.load().grid.clone()
    }

    pub(crate) fn status(&self) -> SyncStatusSnapshot {
        self.0.load().status
    }

    pub(crate) fn topology(&self) -> Result<SyncGroupSnapshot, SyncError> {
        self.0.load().topology.clone()
    }

    fn publish(&self, root: &GroupState<PlayerMember>) {
        self.0.store(Arc::new(RootSnapshot {
            grid: root.snapshot(),
            status: root.status(),
            topology: root.topology(),
        }));
    }
}

pub(crate) struct SessionState<B: AudioBackend, S> {
    pub(super) ctx: Option<FirewheelCtx<B>>,
    pub(super) transport_control: Option<TransportControl>,
    pub(super) mix_tap: Option<MixTap>,
    pub(super) session_limiter_node_id: Option<NodeID>,
    pub(super) session_output_memo: Option<Memo<VolumeNode>>,
    pub(super) session_output_node_id: Option<NodeID>,
    pub(super) next_player_id: PlayerId,
    pub(super) session_ducking: SessionDuckingMode,
    pub(super) start_stream_fn: StartStreamFn<B>,
    pub(super) stream_needs_restart: bool,
    pub(super) requested_sample_rate: u32,
    pub(super) requested_shape: StreamShape,
    pub(super) transport: SessionTransportState,
    pub(super) reserved_session_grid: Option<SessionGridGeneration>,
    pub(super) root: GroupState<PlayerMember>,
    pub(super) root_view: RootView,
    pub(super) graph: GraphRegistry<S>,
}

impl<B: AudioBackend, S> SessionState<B, S> {
    #[cfg(test)]
    pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 44_100;

    /// Creates session state with its own musical-grid topology.
    #[must_use]
    pub(crate) fn new<F>(
        root: GroupState<PlayerMember>,
        root_view: RootView,
        requested_shape: StreamShape,
        start_stream_fn: F,
    ) -> Self
    where
        F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
    {
        let grid_id = root.id();
        let mut generation = SessionGridGeneration::new(grid_id);
        generation.commit_revision(BeatGridRevision::first());
        Self {
            start_stream_fn: Box::new(start_stream_fn),
            ctx: None,
            transport_control: None,
            mix_tap: None,
            next_player_id: 1,
            requested_sample_rate: requested_shape.sample_rate.get(),
            requested_shape,
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
            session_limiter_node_id: None,
            stream_needs_restart: false,
            transport: SessionTransportState::default(),
            reserved_session_grid: Some(generation),
            root,
            root_view,
            graph: GraphRegistry::default(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) const fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.ctx.as_mut()
    }

    pub(super) fn publish_root(&self) {
        self.root_view.publish(&self.root);
    }
}

pub(super) fn register_player<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
    grid_id: BeatGridId,
    bus: EventBus,
    eq_layout: Vec<EqBandConfig>,
    pools: PoolRegion<S>,
    sample_rate: u32,
) -> Result<PlayerId, SessionError> {
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
    let deck = Deck::new(player_id, grid_id, bus, eq_layout, pools, master_volume);
    state.graph.insert(deck)?;
    state.next_player_id = next_player_id;
    debug!(
        player_id,
        players = state.graph.len(),
        "[KITHARA-ROUTE] session player registered"
    );
    Ok(player_id)
}

pub(super) fn ensure_ctx<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    ensure_stream_ready(state, sample_rate)?;
    ensure_session_output(state)
}

fn ensure_stream_ready<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
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

fn create_firewheel_context<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    let shape_sample_rate =
        NonZeroU32::new(sample_rate).unwrap_or(state.requested_shape.sample_rate);
    debug!(sample_rate, "[KITHARA-ROUTE] creating firewheel context");
    let config = FirewheelConfig {
        num_graph_outputs: ChannelCount::STEREO,
        ..FirewheelConfig::default()
    };
    let mut ctx = FirewheelCtx::<B>::new(config);
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
    if let Err(error) = (state.start_stream_fn)(&mut ctx, sample_rate) {
        state.reserved_session_grid = Some(session_grid);
        return Err(SessionError::StreamStart(error));
    }
    state.ctx = Some(ctx);
    state.transport_control = Some(transport_control);
    state.requested_sample_rate = sample_rate;
    state.requested_shape.sample_rate = shape_sample_rate;
    state.stream_needs_restart = false;
    trace_stream_info(state, "start-stream");
    debug!(sample_rate, "[KITHARA-ROUTE] firewheel context ready");
    Ok(())
}

fn ensure_session_output<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
) -> Result<(), SessionError> {
    if state.session_output_node_id.is_none() {
        return create_session_output(state);
    }

    Ok(())
}

fn create_session_output<B: AudioBackend, S>(
    state: &mut SessionState<B, S>,
) -> Result<(), SessionError> {
    debug!("[KITHARA-ROUTE] creating session output graph");
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err(SessionError::NoContext);
    };
    let session_node = VolumeNode::from_linear(ducking_gain(state.session_ducking));
    let session_memo = Memo::new(session_node);
    let session_id = fw_ctx.add_node(session_node, None);
    let limiter_id = fw_ctx.add_node(LimiterNode, None);
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
