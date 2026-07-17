#[rustfmt::skip]
use firewheel::nodes::volume_pan::VolumePanNode;
use firewheel::{
    FirewheelConfig, FirewheelCtx, Volume, backend::AudioBackend, channel_config::ChannelCount,
    diff::Memo, node::NodeID,
};
use kithara_audio::EqBandConfig;
use kithara_bufpool::PcmPool;
use tracing::{debug, warn};

use super::{
    dispatch::{restart_stream, trace_stream_info},
    graph::ducking_gain,
    protocol::{PlayerId, SessionError, StartStreamFn},
};
use crate::{
    api::{SessionDuckingMode, SlotId},
    bridge::SharedEq,
    rt::{MasterEqNode, RenderContextControl, SessionTransportCommit, install_render_context},
};

#[derive(Debug)]
pub(super) struct SlotNodes {
    pub(super) vol_pan_memo: Memo<VolumePanNode>,
    pub(super) player_node_id: NodeID,
    pub(super) vol_pan_node_id: NodeID,
    pub(super) slot_id: SlotId,
}

pub(super) struct PlayerState {
    pub(super) master_eq_memo: Option<Memo<MasterEqNode>>,
    pub(super) master_eq_node_id: Option<NodeID>,
    pub(super) master_vol_pan_memo: Option<Memo<VolumePanNode>>,
    pub(super) master_vol_pan_node_id: Option<NodeID>,
    pub(super) pcm_pool: PcmPool,
    pub(super) player_id: PlayerId,
    pub(super) shared_eq: SharedEq,
    pub(super) eq_layout: Vec<EqBandConfig>,
    pub(super) slots: Vec<SlotNodes>,
    pub(super) started: bool,
    pub(super) master_volume: f32,
    pub(super) next_slot_id: u64,
}

impl PlayerState {
    fn new(player_id: PlayerId, eq_layout: Vec<EqBandConfig>, pcm_pool: PcmPool) -> Self {
        let band_count = eq_layout.len();
        Self {
            eq_layout,
            pcm_pool,
            player_id,
            master_eq_memo: None,
            master_eq_node_id: None,
            master_volume: 1.0,
            master_vol_pan_memo: None,
            master_vol_pan_node_id: None,
            next_slot_id: 1,
            shared_eq: SharedEq::new(band_count),
            slots: Vec::new(),
            started: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransportCommitPhase {
    Aborting,
    Applying,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingTransportCommit {
    pub(super) phase: TransportCommitPhase,
    pub(super) revision: u64,
}

#[derive(Debug, Default)]
pub(super) struct SessionTransportState {
    pub(super) accepted: Option<SessionTransportCommit>,
    pub(super) completed_revision: u64,
    pub(super) last_revision: u64,
    pub(super) observed: Option<SessionTransportCommit>,
    pub(super) pending: Option<PendingTransportCommit>,
    pub(super) rejected: Option<u64>,
}

pub struct SessionState<B: AudioBackend> {
    pub(super) ctx: Option<FirewheelCtx<B>>,
    pub(super) render_context_control: Option<RenderContextControl>,
    pub(super) session_output_memo: Option<Memo<VolumePanNode>>,
    pub(super) session_output_node_id: Option<NodeID>,
    pub(super) next_player_id: PlayerId,
    pub(super) session_ducking: SessionDuckingMode,
    pub(super) start_stream_fn: StartStreamFn<B>,
    pub(super) players: Vec<PlayerState>,
    pub(super) stream_needs_restart: bool,
    pub(super) sample_rate_hint: u32,
    pub(super) transport: SessionTransportState,
}

impl<B: AudioBackend> SessionState<B> {
    pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;

    #[must_use]
    pub fn new(start_stream_fn: StartStreamFn<B>) -> Self {
        Self {
            start_stream_fn,
            ctx: None,
            render_context_control: None,
            next_player_id: 1,
            players: Vec::new(),
            sample_rate_hint: Self::DEFAULT_SAMPLE_RATE,
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
            stream_needs_restart: false,
            transport: SessionTransportState::default(),
        }
    }

    pub fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.ctx.as_mut()
    }
}

pub(super) fn register_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    eq_layout: Vec<EqBandConfig>,
    pcm_pool: PcmPool,
) -> PlayerId {
    let player_id = state.next_player_id;
    state.next_player_id += 1;
    state
        .players
        .push(PlayerState::new(player_id, eq_layout, pcm_pool));
    debug!(
        player_id,
        players = state.players.len(),
        "[KITHARA-ROUTE] session player registered"
    );
    player_id
}

pub(super) fn ensure_ctx<B: AudioBackend>(
    state: &mut SessionState<B>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    ensure_stream_ready(state, sample_rate)?;
    ensure_session_output(state)
}

fn ensure_stream_ready<B: AudioBackend>(
    state: &mut SessionState<B>,
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

fn create_firewheel_context<B: AudioBackend>(
    state: &mut SessionState<B>,
    sample_rate: u32,
) -> Result<(), SessionError> {
    debug!(sample_rate, "[KITHARA-ROUTE] creating firewheel context");
    let config = FirewheelConfig {
        num_graph_outputs: ChannelCount::STEREO,
        ..FirewheelConfig::default()
    };
    let mut ctx = FirewheelCtx::<B>::new(config);
    let render_context_control = install_render_context(&mut ctx)
        .map_err(|reason| SessionError::Graph(String::from(reason)))?;
    (state.start_stream_fn)(&mut ctx, sample_rate).map_err(SessionError::StreamStart)?;
    state.ctx = Some(ctx);
    state.render_context_control = Some(render_context_control);
    state.sample_rate_hint = sample_rate;
    state.stream_needs_restart = false;
    trace_stream_info(state, "start-stream");
    debug!(sample_rate, "[KITHARA-ROUTE] firewheel context ready");
    Ok(())
}

fn ensure_session_output<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    if state.session_output_node_id.is_none() {
        return create_session_output(state);
    }

    Ok(())
}

fn create_session_output<B: AudioBackend>(state: &mut SessionState<B>) -> Result<(), SessionError> {
    debug!("[KITHARA-ROUTE] creating session output graph");
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err(SessionError::NoContext);
    };
    let session_node =
        VolumePanNode::from_volume(Volume::Linear(ducking_gain(state.session_ducking)));
    let session_memo = Memo::new(session_node);
    let session_id = fw_ctx.add_node(session_node, None);
    let graph_out = fw_ctx.graph_out_node_id();
    fw_ctx
        .connect(session_id, graph_out, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect session output to graph_out failed: {err}"))
        })?;
    if let Err(err) = fw_ctx.update() {
        warn!("session graph update after output init failed: {err:?}");
    }
    state.session_output_node_id = Some(session_id);
    state.session_output_memo = Some(session_memo);
    debug!(?session_id, "[KITHARA-ROUTE] session output graph ready");
    Ok(())
}
