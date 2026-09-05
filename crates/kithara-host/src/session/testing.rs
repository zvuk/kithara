use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_audio::ConsumerWakeMode;
#[cfg(test)]
use kithara_bufpool::testing::{TestPools, pools};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_output::OutputGroup;
use kithara_platform::sync::Arc;
#[cfg(target_arch = "wasm32")]
use kithara_play::player::PlayerControlSource;
use kithara_play::{
    GroupState, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl,
    SessionDuckingMode, player::PlayerMember,
};
use kithara_warp::{
    BeatGridId, SessionEpoch, SyncAdmission, SyncGroup, SyncMember, SyncMemberKind, SyncOperation,
    TopologyOperation, TransportRevision,
};

use super::{
    dispatch::{restart_stream, run_cmd},
    graph::tap,
    protocol::{Cmd, Reply, SessionDispatcher},
    state::{RootView, SessionState},
};
use crate::Host;

/// Probe-only access to session-output policy.
pub trait HostProbe {
    /// # Errors
    /// Returns an error when the Host cannot read the canonical transport revision.
    fn transport_revision(&self) -> Result<TransportRevision, PlayError>;

    /// # Errors
    /// Returns an error when the Host cannot read the output policy.
    fn ducking(&self) -> Result<SessionDuckingMode, PlayError>;

    /// # Errors
    /// Returns an error when the Host rejects the output-policy update.
    fn set_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError>;

    /// # Errors
    /// Returns an error when the deterministic Host route cannot restart.
    fn restart_stream(&self, sample_rate: u32) -> Result<(), PlayError>;
}

impl<S> HostProbe for Host<S> {
    delegate::delegate! {
        to self {
            fn transport_revision(&self) -> Result<TransportRevision, PlayError>;
            #[call(ducking_mode)]
            fn ducking(&self) -> Result<SessionDuckingMode, PlayError>;
            #[call(set_ducking_mode)]
            fn set_ducking(&self, mode: SessionDuckingMode) -> Result<(), PlayError>;
            fn restart_stream(&self, sample_rate: u32) -> Result<(), PlayError>;
        }
    }
}

/// Test-only owner for the real Host graph running on an injected backend.
///
/// The production Host surface never exposes its raw session state. This
/// probe keeps existing deterministic backend tests on the same graph code.
pub struct GraphSession<B: AudioBackend, S> {
    state: SessionState<B, S>,
}

impl<B, S> GraphSession<B, S>
where
    B: AudioBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub const DEFAULT_SAMPLE_RATE: NonZeroU32 =
        match NonZeroU32::new(SessionState::<B, S>::DEFAULT_SAMPLE_RATE) {
            Some(sample_rate) => sample_rate,
            None => unreachable!(),
        };

    #[must_use]
    pub fn new<F>(start_stream_fn: F) -> Self
    where
        F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
    {
        Self::with_sample_rate(Self::DEFAULT_SAMPLE_RATE, start_stream_fn)
    }

    #[must_use]
    pub fn with_sample_rate<F>(sample_rate: NonZeroU32, start_stream_fn: F) -> Self
    where
        F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
    {
        Self {
            state: state_for(sample_rate, start_stream_fn),
        }
    }

    #[must_use]
    pub fn exec(&mut self, cmd: Cmd<S>) -> Reply {
        if let Cmd::RegisterPlayer { grid_id, pools, .. } = &cmd
            && self.state.root.with_group(*grid_id, |_| ()).is_none()
        {
            attach_player_with_id(&mut self.state, *grid_id, pools.clone());
        }
        run_cmd(&mut self.state, cmd)
    }

    pub fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.state.ctx.as_mut()
    }

    /// Install the real post-limiter output node in a deterministic graph.
    ///
    /// # Errors
    /// Returns an error when an output is already active or graph installation fails.
    pub fn enable_outputs(&mut self, outputs: OutputGroup) -> Result<(), PlayError> {
        tap::enable(&mut self.state, outputs).map_err(Into::into)
    }

    /// Restart the deterministic graph at a different output rate.
    ///
    /// # Errors
    /// Returns an error when the existing Host route cannot restart.
    pub fn restart_stream(&mut self, sample_rate: u32) -> Result<(), PlayError> {
        restart_stream(&mut self.state, sample_rate).map_err(Into::into)
    }
}

pub(crate) struct FixtureSession;

impl<S> SessionDispatcher<S> for FixtureSession {
    fn exec(&self, _cmd: Cmd<S>) -> Result<Reply, PlayError> {
        Ok(Reply::Ok)
    }

    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }
}

#[cfg(test)]
pub(crate) fn state<B, F>(start_stream_fn: F) -> SessionState<B, TestPools>
where
    B: AudioBackend,
    F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
{
    state_for(
        GraphSession::<B, TestPools>::DEFAULT_SAMPLE_RATE,
        start_stream_fn,
    )
}

fn state_for<B, F, S>(sample_rate: NonZeroU32, start_stream_fn: F) -> SessionState<B, S>
where
    B: AudioBackend,
    F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
{
    let grid_id = BeatGridId::allocate().expect("fixture host grid id");
    let root = GroupState::unavailable(
        grid_id,
        sample_rate,
        SessionEpoch::new(0),
        SyncMemberKind::Group,
    );
    let root_view = RootView::new(&root);
    SessionState::new(root, root_view, sample_rate, None, start_stream_fn)
}

#[cfg(test)]
pub(crate) fn attach_player<B: AudioBackend>(state: &mut SessionState<B, TestPools>) -> BeatGridId {
    let grid_id = BeatGridId::allocate().expect("fixture player grid id");
    attach_player_with_id(state, grid_id, pools());
    grid_id
}

fn attach_player_with_id<B, S>(
    state: &mut SessionState<B, S>,
    grid_id: BeatGridId,
    pools: PoolRegion<S>,
) where
    B: AudioBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools).build());
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .grid_id(grid_id)
            .sample_rate(state.root_view.grid().axis().sample_rate())
            .worker(worker)
            .session(Arc::new(FixtureSession))
            .build(),
    );
    let base = state
        .root
        .topology()
        .expect("fixture host topology")
        .stamp();
    let admission = state
        .root
        .transact(SyncOperation::Topology {
            base,
            operations: Box::new([TopologyOperation::Attach {
                member: SyncMember::Group {
                    alignment: None,
                    group: Box::new(target_member(player)),
                },
            }]),
        })
        .expect("fixture player attachment");
    assert!(matches!(admission, SyncAdmission::TopologyChanged { .. }));
    state.publish_root();
}

#[cfg(all(test, target_arch = "wasm32"))]
pub(crate) fn fixture_member(grid_id: BeatGridId, sample_rate: NonZeroU32) -> PlayerMember {
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .grid_id(grid_id)
            .sample_rate(sample_rate)
            .worker(worker)
            .session(Arc::new(FixtureSession))
            .build(),
    );
    target_member(player)
}

#[cfg(not(target_arch = "wasm32"))]
fn target_member<S>(player: PlayerImpl<S>) -> PlayerMember
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    PlayerMember::new(player)
}

#[cfg(target_arch = "wasm32")]
fn target_member<S>(mut player: PlayerImpl<S>) -> PlayerMember
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    player
        .take_host_member()
        .expect("fixture player synchronization member")
}
