#[rustfmt::skip]
use firewheel::nodes::volume_pan::VolumePanNode;
use firewheel::{
    FirewheelConfig, FirewheelCtx, Volume, backend::AudioBackend, channel_config::ChannelCount,
    diff::Memo, node::NodeID,
};
use kithara_audio::EqBandConfig;
use kithara_bufpool::PcmPool;
use kithara_events::EventBus;
use tracing::{debug, warn};

use super::{
    dispatch::{restart_stream, trace_stream_info},
    eq::MasterEqNode,
    graph::ducking_gain,
    protocol::{PlayerId, SessionError, SessionRosterRevision, StartStreamFn},
    render::{RenderContextControl, SessionTransportCommit, install_render_context},
};
use crate::{
    api::{SessionDuckingMode, SlotId, TransportRevision},
    bridge::SharedEq,
};

#[derive(Debug)]
pub(super) struct SlotNodes {
    pub(super) vol_pan_memo: Memo<VolumePanNode>,
    pub(super) player_node_id: NodeID,
    pub(super) vol_pan_node_id: NodeID,
    pub(super) slot_id: SlotId,
}

pub(super) struct PlayerState {
    pub(super) bus: EventBus,
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
    fn new(
        player_id: PlayerId,
        bus: EventBus,
        eq_layout: Vec<EqBandConfig>,
        pcm_pool: PcmPool,
    ) -> Self {
        let band_count = eq_layout.len();
        Self {
            bus,
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
pub(super) enum AbortDelivery {
    Pending,
    Sent,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TransportLedger {
    pub(super) completed: Option<TransportRevision>,
    pub(super) last: Option<TransportRevision>,
    pub(super) rejected: Option<TransportRevision>,
}

#[derive(Debug)]
pub(super) enum SessionTransportState {
    Unconfigured {
        ledger: TransportLedger,
    },
    Stable {
        active: SessionTransportCommit,
        ledger: TransportLedger,
    },
    Applying {
        ledger: TransportLedger,
        next: SessionTransportCommit,
        previous: Option<SessionTransportCommit>,
    },
    Aborting {
        delivery: AbortDelivery,
        ledger: TransportLedger,
        previous: Option<SessionTransportCommit>,
        revision: TransportRevision,
    },
}

impl Default for SessionTransportState {
    fn default() -> Self {
        Self::Unconfigured {
            ledger: TransportLedger::default(),
        }
    }
}

impl SessionTransportState {
    pub(super) const fn accepted(&self) -> Option<SessionTransportCommit> {
        match self {
            Self::Unconfigured { .. } => None,
            Self::Stable { active, .. } => Some(*active),
            Self::Applying { next, .. } => Some(*next),
            Self::Aborting { previous, .. } => *previous,
        }
    }

    pub(super) const fn ledger(&self) -> &TransportLedger {
        match self {
            Self::Unconfigured { ledger }
            | Self::Stable { ledger, .. }
            | Self::Applying { ledger, .. }
            | Self::Aborting { ledger, .. } => ledger,
        }
    }

    pub(super) const fn ledger_mut(&mut self) -> &mut TransportLedger {
        match self {
            Self::Unconfigured { ledger }
            | Self::Stable { ledger, .. }
            | Self::Applying { ledger, .. }
            | Self::Aborting { ledger, .. } => ledger,
        }
    }

    pub(super) const fn observed(&self) -> Option<SessionTransportCommit> {
        match self {
            Self::Unconfigured { .. } => None,
            Self::Stable { active, .. } => Some(*active),
            Self::Applying { previous, .. } | Self::Aborting { previous, .. } => *previous,
        }
    }

    pub(super) const fn pending_revision(&self) -> Option<TransportRevision> {
        match self {
            Self::Applying { next, .. } => Some(next.revision()),
            Self::Aborting { revision, .. } => Some(*revision),
            Self::Unconfigured { .. } | Self::Stable { .. } => None,
        }
    }
}

pub struct SessionState<B: AudioBackend> {
    pub(super) ctx: Option<FirewheelCtx<B>>,
    pub(super) render_context_control: Option<RenderContextControl>,
    pub(super) session_output_memo: Option<Memo<VolumePanNode>>,
    pub(super) session_output_node_id: Option<NodeID>,
    pub(super) next_player_id: Option<PlayerId>,
    pub(super) roster_revision: SessionRosterRevision,
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
            next_player_id: Some(PlayerId::FIRST),
            players: Vec::new(),
            roster_revision: SessionRosterRevision::default(),
            sample_rate_hint: Self::DEFAULT_SAMPLE_RATE,
            session_ducking: SessionDuckingMode::Off,
            session_output_memo: None,
            session_output_node_id: None,
            stream_needs_restart: false,
            transport: SessionTransportState::default(),
        }
    }

    pub(super) fn commit_roster_revision(&mut self, revision: SessionRosterRevision) {
        self.roster_revision = revision;
    }

    pub fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.ctx.as_mut()
    }

    pub(super) fn next_roster_revision(&self) -> Result<SessionRosterRevision, SessionError> {
        self.roster_revision
            .checked_next()
            .ok_or(SessionError::SessionRosterRevisionExhausted)
    }
}

pub(super) fn register_player<B: AudioBackend>(
    state: &mut SessionState<B>,
    bus: EventBus,
    eq_layout: Vec<EqBandConfig>,
    pcm_pool: PcmPool,
) -> Result<PlayerId, SessionError> {
    let player_id = state
        .next_player_id
        .ok_or(SessionError::PlayerIdExhausted)?;
    let roster_revision = state.next_roster_revision()?;
    state.next_player_id = player_id.checked_next();
    state.roster_revision = roster_revision;
    state
        .players
        .push(PlayerState::new(player_id, bus, eq_layout, pcm_pool));
    debug!(
        player_id = player_id.get(),
        players = state.players.len(),
        "[KITHARA-ROUTE] session player registered"
    );
    Ok(player_id)
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
