use std::num::NonZeroU32;

use arc_swap::ArcSwap;
use firewheel::{
    FirewheelConfig, FirewheelContext,
    channel_config::ChannelCount,
    node::{AudioNode, NodeID},
};
use kithara_command::{Live, ScopedConfig, ScopedSender, Seq, When};
use kithara_config::ConfigOwner;
use kithara_events::EventBus;
use kithara_output::OutputGroup;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_play::{PlayError, SessionOutputView, SessionSampleRate, StreamShape};
use kithara_render::bridge::DeckProtocol;
use kithara_signal::{FrameCount, SessionEpoch, SessionFrame};
use kithara_warp::{
    BeatGridId, BeatGridRevision, BeatGridSnapshot, BeatGridStamp, MapAxis, SessionAxis,
};
use tracing::{debug, warn};
use triple_buffer::Output;

use super::{
    dispatch::{restart_stream, sample_rate, stream_shape, trace_stream_info},
    graph::tap,
    protocol::{SessionError, StartStreamFn},
    queue::HostProtocol,
    transport::{SessionGridGeneration, SessionInboxReturnNode, TransportObservation, install},
};
use crate::{
    DeckId,
    api::Tap,
    host::HostSettings,
    rt::{MasterNode, SessionOutput},
};

pub(crate) enum TapSlot {
    Requested(OutputGroup),
    Installed(NodeID),
}

#[derive(Default)]
pub(crate) struct Taps {
    master: Option<TapSlot>,
    output: Option<TapSlot>,
}

impl Taps {
    pub(crate) fn slot(&mut self, tap: Tap) -> &mut Option<TapSlot> {
        match tap {
            Tap::Master => &mut self.master,
            Tap::Output => &mut self.output,
        }
    }
}

/// The Host's session grid, which its transport publishes into.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct HostRoot {
    /// The session grid the transport last committed.
    #[field(get, vis = "pub(crate)")]
    grid: BeatGridSnapshot,
}

impl HostRoot {
    /// A root `id` with no deck, its grid not yet live on a session axis at
    /// `sample_rate`.
    pub(crate) fn new(id: BeatGridId, sample_rate: NonZeroU32) -> Self {
        Self {
            grid: BeatGridSnapshot::unavailable(
                id,
                BeatGridRevision::first(),
                MapAxis::Session(SessionAxis::new(sample_rate, SessionEpoch::new(0))),
            ),
        }
    }

    pub(crate) fn id(&self) -> BeatGridId {
        self.grid.id()
    }

    /// Takes the session grid the transport committed.
    pub(crate) fn publish(&mut self, grid: BeatGridSnapshot) {
        self.grid = grid;
    }

    /// Takes the grid of a route boundary: a later revision `stamp` names,
    /// on the session axis of `epoch` at `sample_rate`, with no geometry
    /// until the transport commits one.
    pub(crate) fn publish_unavailable(
        &mut self,
        stamp: BeatGridStamp,
        sample_rate: NonZeroU32,
        epoch: SessionEpoch,
    ) {
        self.publish(BeatGridSnapshot::unavailable(
            stamp.grid_id(),
            stamp.revision(),
            MapAxis::Session(SessionAxis::new(sample_rate, epoch)),
        ));
    }
}

struct RootSnapshot {
    /// Completion fence unavailable until mailbox posts expose their ticket sequence.
    applied: Option<Seq>,
    decks: Box<[BeatGridId]>,
    grid: BeatGridSnapshot,
    settings: HostSettings,
}

/// What the session last published: its decks, grid and settings, and the
/// output its decks read.
#[derive(Clone)]
pub(crate) struct RootView {
    root: Arc<ArcSwap<RootSnapshot>>,
    pub(crate) output: SessionOutputView,
}

impl RootView {
    pub(crate) fn new(root: &HostRoot, settings: HostSettings) -> Self {
        Self {
            root: Arc::new(ArcSwap::from_pointee(RootSnapshot {
                applied: None,
                settings,
                decks: Box::default(),
                grid: root.grid.clone(),
            })),
            output: SessionOutputView::new(settings.sample_rate()),
        }
    }

    fn publish(
        &self,
        root: &HostRoot,
        settings: HostSettings,
        stream_shape: Option<StreamShape>,
        sample_rate: SessionSampleRate,
    ) {
        let snapshot = self.root.load();
        self.root.store(Arc::new(RootSnapshot {
            applied: snapshot.applied,
            settings,
            decks: snapshot.decks.clone(),
            grid: root.grid.clone(),
        }));
        self.output.publish(sample_rate, stream_shape);
    }

    pub(crate) fn publish_decks(&self, decks: Box<[BeatGridId]>) {
        let snapshot = self.root.load();
        self.root.store(Arc::new(RootSnapshot {
            applied: snapshot.applied,
            decks,
            grid: snapshot.grid.clone(),
            settings: snapshot.settings,
        }));
    }

    /// Whether the deck `grid_id` is in the session.
    pub(crate) fn holds(&self, grid_id: BeatGridId) -> bool {
        self.root.load().decks.contains(&grid_id)
    }

    /// Whether the session holds no deck.
    pub(crate) fn is_empty(&self) -> bool {
        self.root.load().decks.is_empty()
    }

    delegate::delegate! {
        to self.root {
            #[call(load)]
            #[expr($.grid.clone())]
            pub(crate) fn grid(&self) -> BeatGridSnapshot;
            #[call(load)]
            #[expr($.settings)]
            pub(crate) fn settings(&self) -> HostSettings;
        }
        to self.output {
            #[call(get)]
            #[expr($.sample_rate)]
            pub(crate) fn sample_rate(&self) -> SessionSampleRate;
        }
    }
}

pub(crate) enum SessionStream {
    #[cfg(not(target_arch = "wasm32"))]
    Realtime {
        _backend: Box<firewheel::cpal::CpalStream>,
    },
    #[cfg(target_arch = "wasm32")]
    Realtime {
        _backend: firewheel_web_audio::WebAudioBackend,
    },
    #[cfg(feature = "offline")]
    Offline(Box<crate::session::offline::backend::OfflineStream>),
}

pub(crate) struct DeckNode {
    pub(crate) id: DeckId,
    pub(crate) node: NodeID,
    pub(crate) bus: Option<EventBus>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SessionBufferConfig {
    pub(crate) max_block_frames: Option<NonZeroU32>,
    pub(crate) declick_frames: Option<NonZeroU32>,
}

pub(crate) struct SessionState<T, S> {
    pub(crate) settled: Vec<crate::HostSettled>,
    /// The single clock read and delivery lead for the current owner pass.
    pub(crate) iteration_clock: Option<(SessionFrame, FrameCount)>,
    /// Wall-time delivery lead, absent for a runtime that drains before rendering.
    pub(crate) delivery_delay: Option<Duration>,
    /// Graph node identities; deck state remains with the deck's owner.
    pub(crate) deck_nodes: Vec<DeckNode>,
    pub(crate) channel_config: ScopedConfig,
    marker: std::marker::PhantomData<fn() -> S>,
    pub(crate) root: HostRoot,
    pub(crate) output: SessionOutput,
    pub(crate) session_metronome_node_id: Option<NodeID>,
    pub(crate) ctx: Option<FirewheelContext>,
    pub(crate) taps: Taps,
    /// The pause/resume fade length the session asks Firewheel for, in frames.
    /// `None` leaves Firewheel's own default in place.
    pub(crate) requested_declick_frames: Option<NonZeroU32>,
    pub(crate) requested_max_block_frames: Option<NonZeroU32>,
    pub(crate) reserved_session_grid: Option<SessionGridGeneration>,
    pub(crate) session_limiter_node_id: Option<NodeID>,
    pub(crate) session_output_node_id: Option<NodeID>,
    pub(crate) stream: Option<T>,
    /// The root and deck scopes of the session's command channel.
    pub(crate) channel: Option<ScopedSender<HostProtocol, DeckProtocol>>,
    /// What the running transport last committed.
    pub(crate) transport_observation: Option<Output<TransportObservation>>,
    pub(crate) root_view: RootView,
    /// The Host settings as the render graph confirmed them, with the
    /// changes still on their way to it.
    pub(crate) settings: Live<HostSettings, HostProtocol>,
    pub(crate) start_stream_fn: StartStreamFn<T>,
    /// Set when the output device is acquired once and cannot be rebuilt, so
    /// an idle session must keep it rather than release it.
    pub(crate) retains_output: bool,
    pub(crate) stream_needs_restart: bool,
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
    /// Creates session state with its own musical-grid topology, asking for
    /// the output at the sample rate its settings name.
    #[must_use]
    pub(crate) fn new<F>(
        root: HostRoot,
        root_view: RootView,
        buffers: SessionBufferConfig,
        output: SessionOutput,
        settings: Live<HostSettings, HostProtocol>,
        channel_config: ScopedConfig,
        start_stream_fn: F,
    ) -> Self
    where
        F: FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static,
    {
        let grid_id = root.id();
        let mut generation = SessionGridGeneration::new(grid_id);
        generation.commit_revision(BeatGridRevision::first());
        let state = Self {
            settings,
            channel_config,
            requested_max_block_frames: buffers.max_block_frames,
            requested_declick_frames: buffers.declick_frames,
            output,
            session_metronome_node_id: None,
            root,
            root_view,
            start_stream_fn: Box::new(start_stream_fn),
            ctx: None,
            stream: None,
            channel: None,
            transport_observation: None,
            taps: Taps::default(),
            session_output_node_id: None,
            session_limiter_node_id: None,
            retains_output: false,
            stream_needs_restart: false,
            reserved_session_grid: Some(generation),
            settled: Vec::new(),
            iteration_clock: None,
            delivery_delay: None,
            deck_nodes: Vec::new(),
            marker: std::marker::PhantomData,
        };
        state.publish_root();
        state
    }

    pub(crate) fn begin_iteration(&mut self) {
        self.iteration_clock = self.ctx.as_ref().and_then(|ctx| {
            let _ = ctx.stream_info()?;
            Some((
                SessionFrame::new(ctx.audio_clock().samples.0),
                self.delivery(),
            ))
        });
    }

    /// Rejects a timed change that cannot reach the render executor in this pass.
    pub(crate) fn check_when(&self, when: When<SessionFrame>) -> Result<(), PlayError> {
        match when {
            When::Next => Ok(()),
            When::At(frame) => {
                let (now, delivery) = self.iteration_clock.ok_or(PlayError::Untimed)?;
                if frame < now + delivery {
                    Err(PlayError::Late)
                } else {
                    Ok(())
                }
            }
            When::Deferred => Err(PlayError::Internal(
                "this owner change cannot be deferred".to_owned(),
            )),
        }
    }

    /// The configured owner pump and worker wake allowances, plus one output block.
    pub(crate) fn delivery(&self) -> FrameCount {
        let duration = self.delivery_delay.unwrap_or_default();
        let publication_frames = duration
            .as_nanos()
            .saturating_mul(u128::from(sample_rate(self).output()))
            .div_ceil(1_000_000_000);
        let publication_frames = usize::try_from(publication_frames).unwrap_or(usize::MAX);
        let block_frames =
            stream_shape(self).map_or(0, |shape| shape.max_block_frames.get() as usize);
        FrameCount::new(publication_frames.saturating_add(block_frames))
    }

    pub(crate) fn publish_root(&self) {
        self.root_view.publish(
            &self.root,
            *self.settings.config(),
            stream_shape(self),
            sample_rate(self),
        );
    }
}

/// Adds a node to the graph, turning the rejection Firewheel now reports into
/// the session's own graph error. A node the graph refuses is a wiring bug, not
/// a runtime condition the session can route around.
pub(crate) fn add_graph_node<N: AudioNode + 'static>(
    ctx: &mut FirewheelContext,
    node: N,
) -> Result<NodeID, SessionError> {
    ctx.add_node(node, None)
        .map_err(|err| SessionError::Graph(format!("audio graph rejected a node: {err}")))
}

pub(crate) fn ensure_ctx<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    ensure_stream_ready(state)?;
    ensure_session_output(state)
}

fn ensure_stream_ready<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    if state.ctx.is_none() {
        return create_firewheel_context(state);
    }

    if state.stream_needs_restart || state.stream.is_none() {
        debug!("[KITHARA-ROUTE] ensuring stopped stream is restarted");
        restart_stream(state)?;
    }

    Ok(())
}

/// Converts the fade through `Duration` rather than casting directly, since Firewheel takes the
/// fade in seconds while the frame count is the session's own unit.
fn create_firewheel_context<T, S>(state: &mut SessionState<T, S>) -> Result<(), SessionError> {
    let sample_rate = state.settings.config().sample_rate().get();
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
    let (channel, transport_observation) = match install(
        &mut ctx,
        session_grid,
        *state.settings.config(),
        state.channel_config,
    ) {
        Ok(transport) => transport,
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
    state.channel = Some(channel);
    state.transport_observation = Some(transport_observation);
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
    let limiter = state.output.limiter();
    let metronome = state.output.metronome(state.settings.config().metronome());
    let Some(ref mut fw_ctx) = state.ctx else {
        return Err(SessionError::NoContext);
    };
    let session_id = add_graph_node(fw_ctx, MasterNode)?;
    let limiter_id = add_graph_node(fw_ctx, limiter)?;
    let metronome_id = add_graph_node(fw_ctx, metronome)?;
    let inbox_return_id = add_graph_node(fw_ctx, SessionInboxReturnNode)?;
    let graph_out = fw_ctx.graph_out_node_id();
    fw_ctx
        .connect(session_id, limiter_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect session output to limiter failed: {err}"))
        })?;
    fw_ctx
        .connect(limiter_id, metronome_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect limiter to metronome failed: {err}"))
        })?;
    fw_ctx
        .connect(metronome_id, inbox_return_id, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect metronome to inbox return failed: {err}"))
        })?;
    fw_ctx
        .connect(inbox_return_id, graph_out, &[(0, 0), (1, 1)], false)
        .map_err(|err| {
            SessionError::Graph(format!("connect inbox return to graph_out failed: {err}"))
        })?;
    if let Err(err) = fw_ctx.update() {
        warn!("session graph update after output init failed: {err:?}");
    }
    state.session_output_node_id = Some(session_id);
    state.session_limiter_node_id = Some(limiter_id);
    state.session_metronome_node_id = Some(metronome_id);
    tap::install_requested(state)?;
    debug!(
        ?session_id,
        ?limiter_id,
        ?metronome_id,
        "[KITHARA-ROUTE] session output graph ready"
    );
    Ok(())
}
