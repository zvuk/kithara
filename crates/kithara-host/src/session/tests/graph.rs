use std::num::NonZeroU32;

use firewheel::{FirewheelCtx, backend::AudioBackend};
use kithara_audio::ConsumerWakeMode;
#[cfg(test)]
use kithara_bufpool::testing::{TestPools, pools};
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;
#[cfg(target_arch = "wasm32")]
use kithara_play::player::PlayerControlSource;
use kithara_play::{
    GroupState, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, SessionBinding,
    player::PlayerMember,
};
use kithara_warp::{
    BeatGridId, SessionEpoch, SyncAdmission, SyncGroup, SyncMember, SyncMemberKind, SyncOperation,
    TopologyOperation,
};

use super::super::{
    dispatch::run_cmd,
    protocol::{Cmd, Reply, SessionDispatcher},
    state::{RootView, SessionState},
};
/// Test-only owner for the real Host graph running on an injected backend.
///
/// The production Host surface never exposes its raw session state. This
/// probe keeps existing deterministic backend tests on the same graph code.
pub(crate) struct GraphSession<B: AudioBackend, S> {
    state: SessionState<B, S>,
}

impl<B, S> GraphSession<B, S>
where
    B: AudioBackend,
    S: HasPool<f32> + Send + Sync + 'static,
{
    pub(crate) const DEFAULT_SAMPLE_RATE: NonZeroU32 =
        match NonZeroU32::new(SessionState::<B, S>::DEFAULT_SAMPLE_RATE) {
            Some(sample_rate) => sample_rate,
            None => unreachable!(),
        };

    #[must_use]
    pub(crate) fn new<F>(start_stream_fn: F) -> Self
    where
        F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
    {
        Self::with_sample_rate(Self::DEFAULT_SAMPLE_RATE, start_stream_fn)
    }

    pub(crate) fn ctx_mut(&mut self) -> Option<&mut FirewheelCtx<B>> {
        self.state.ctx.as_mut()
    }

    #[must_use]
    pub(crate) fn exec(&mut self, cmd: Cmd<S>) -> Reply {
        if let Cmd::RegisterPlayer { grid_id, pools, .. } = &cmd
            && self.state.root.with_group(*grid_id, |_| ()).is_none()
        {
            attach_player_with_id(&mut self.state, *grid_id, pools.clone());
        }
        run_cmd(&mut self.state, cmd)
    }

    #[must_use]
    fn with_sample_rate<F>(sample_rate: NonZeroU32, start_stream_fn: F) -> Self
    where
        F: FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send + 'static,
    {
        Self {
            state: state_for(sample_rate, start_stream_fn),
        }
    }
}

const FIXTURE_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(rate) => rate,
    None => unreachable!(),
};

pub(crate) struct FixtureSession;

impl<S> SessionDispatcher<S> for FixtureSession {
    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::RealtimeDeferred
    }

    fn exec(&self, _cmd: Cmd<S>) -> Result<Reply, PlayError> {
        Ok(Reply::Ok)
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
    let root_view = RootView::new(&root, sample_rate);
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
            .session(SessionBinding::new(
                Arc::new(FixtureSession),
                FIXTURE_SAMPLE_RATE,
            ))
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
            .session(SessionBinding::new(
                Arc::new(FixtureSession),
                FIXTURE_SAMPLE_RATE,
            ))
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
