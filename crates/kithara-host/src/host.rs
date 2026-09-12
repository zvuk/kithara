use std::{marker::PhantomData, num::NonZeroU32, ops::Deref};

use kithara_bufpool::HasPool;
use kithara_output::OutputGroup;
use kithara_platform::sync::Arc;
use kithara_play::{
    GroupState, PlayError, SessionBinding, SessionDispatcher, Tempo,
    player::{PlayerControlSource, PlayerMember},
};
use kithara_warp::{
    BeatGrid, BeatGridId, SessionEpoch, SyncAdmission, SyncApplied, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncMember, SyncMemberKind, SyncOperation, SyncRejected, SyncStatusSnapshot,
    TopologyOperation,
};
mod config;
#[cfg(feature = "offline")]
mod offline;
mod platform;

pub use config::HostConfig;
#[cfg(feature = "offline")]
use offline::OfflineRuntime;
use platform::{Platform, PlatformResult};

use crate::{
    api::{HostLevel, SessionTransportSnapshot},
    session::{
        Cmd, HostCmd, HostDispatcher, HostReply, Reply, RootView, SessionError, SessionSampleRate,
    },
};

/// Typed command proxy for one player value exclusively resident in a Host.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct HostOwned<P: PlayerControlSource> {
    host_id: BeatGridId,
    #[field(get, copy)]
    id: BeatGridId,
    #[field(get)]
    control: P::Control,
    marker: PhantomData<fn() -> P>,
}

impl<P: PlayerControlSource> HostOwned<P> {
    /// Creates one input for [`Host::apply_mix`].
    #[must_use]
    pub const fn level(&self, level: f32) -> HostLevel {
        HostLevel::new(self.id, level)
    }
}

impl<P: PlayerControlSource> Deref for HostOwned<P> {
    type Target = P::Control;

    fn deref(&self) -> &Self::Target {
        &self.control
    }
}

/// Exclusive owner and dispatcher for one multi-player output session.
pub struct Host<S> {
    dispatcher: Arc<dyn HostDispatcher<S>>,
    id: BeatGridId,
    root_view: RootView,
    session: SessionRuntime<S>,
    owns_session: bool,
}

enum SessionRuntime<S> {
    Realtime(Platform<S>),
    #[cfg(feature = "offline")]
    Offline {
        platform: Platform<S>,
        runtime: OfflineRuntime<S>,
    },
}

impl<S> SessionRuntime<S> {
    #[cfg(feature = "offline")]
    const fn offline(platform: Platform<S>, runtime: OfflineRuntime<S>) -> Self {
        Self::Offline { platform, runtime }
    }

    #[cfg(feature = "offline")]
    const fn offline_runtime_mut(&mut self) -> Option<&mut OfflineRuntime<S>> {
        match self {
            Self::Offline { runtime, .. } => Some(runtime),
            Self::Realtime(_) => None,
        }
    }

    const fn platform(&self) -> &Platform<S> {
        match self {
            Self::Realtime(platform) => platform,
            #[cfg(feature = "offline")]
            Self::Offline { platform, .. } => platform,
        }
    }

    const fn platform_mut(&mut self) -> &mut Platform<S> {
        match self {
            Self::Realtime(platform) => platform,
            #[cfg(feature = "offline")]
            Self::Offline { platform, .. } => platform,
        }
    }

    const fn realtime(platform: Platform<S>) -> Self {
        Self::Realtime(platform)
    }
}

struct SessionRoot {
    id: BeatGridId,
    group: GroupState<PlayerMember>,
    sample_rate: NonZeroU32,
    view: RootView,
}

impl<S> Host<S> {
    /// Applies one validated, atomic batch of final player levels.
    ///
    /// # Errors
    /// Returns an error for invalid members, levels, or graph dispatch failure.
    pub fn apply_mix<I>(&self, levels: I) -> Result<(), PlayError>
    where
        I: IntoIterator<Item = HostLevel>,
    {
        let levels = levels.into_iter().collect();
        match self
            .dispatcher
            .exec_host(HostCmd::ApplyMix { levels })
            .map_err(PlayError::from)?
        {
            HostReply::Ok => Ok(()),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for mix update".into(),
            )),
        }
    }

    fn attach_member(&self, member: PlayerMember) -> Result<(), PlayError> {
        let operations = Box::new([TopologyOperation::Attach {
            member: SyncMember::Group {
                alignment: None,
                group: Box::new(member),
            },
        }]);
        require_topology_change(self.dispatcher.transact_current(operations))
    }

    fn bind_player<P>(&self, player: &mut P) -> Result<(BeatGridId, P::Control), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        let grid_id = player.id();
        let dispatcher: Arc<dyn SessionDispatcher<S>> = self.dispatcher.clone();
        player.attach_session(SessionBinding::new(
            dispatcher,
            self.requested_sample_rate(),
        ))?;
        Ok((grid_id, player.control()))
    }

    fn detach_member(&self, member: BeatGridId) -> Result<(), PlayError> {
        let operations = Box::new([TopologyOperation::Detach { member }]);
        require_topology_change(self.dispatcher.transact_current(operations))
    }

    /// Removes the post-limiter output group.
    ///
    /// # Errors
    /// Returns an error when graph dispatch fails.
    pub fn disable_outputs(&self) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::DisableMixTap)
    }

    /// Installs one post-limiter group for simultaneous independent outputs.
    ///
    /// # Errors
    /// Returns an error when an output group is active or graph dispatch fails.
    pub fn enable_outputs(&self, outputs: OutputGroup) -> Result<(), PlayError> {
        match self
            .dispatcher
            .exec_host(HostCmd::EnableOutput { outputs })?
        {
            HostReply::Ok => Ok(()),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for output group".into(),
            )),
        }
    }

    fn exec_play_ok(&self, cmd: Cmd<S>) -> Result<(), PlayError> {
        match self.dispatcher.exec(cmd)? {
            Reply::Ok => Ok(()),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected host reply for session command".into(),
            )),
        }
    }

    /// Restart the current output route while preserving Host-owned graph state.
    ///
    /// # Errors
    /// Returns an error when the session cannot restart its output route.
    pub fn invalidate_audio_route<R>(&self, reason: R) -> Result<(), PlayError>
    where
        R: Into<String>,
    {
        self.exec_play_ok(Cmd::InvalidateAudioRoute {
            reason: reason.into(),
        })
    }

    fn owned<P>(&self, id: BeatGridId, control: P::Control) -> HostOwned<P>
    where
        P: PlayerControlSource,
    {
        HostOwned {
            id,
            control,
            host_id: self.id,
            marker: PhantomData,
        }
    }

    fn owner(
        id: BeatGridId,
        root_view: RootView,
        dispatcher: Arc<dyn HostDispatcher<S>>,
        session: SessionRuntime<S>,
    ) -> Self {
        Self {
            id,
            root_view,
            dispatcher,
            session,
            owns_session: true,
        }
    }

    /// Returns the session rate used before the output device is measured.
    #[must_use]
    pub fn requested_sample_rate(&self) -> NonZeroU32 {
        self.root_view.grid().axis().sample_rate()
    }

    /// Apply the output rate measured after a platform audio-route change.
    ///
    /// # Errors
    /// Returns an error when the session cannot recreate its output stream.
    pub fn update_audio_route(&self, sample_rate: NonZeroU32) -> Result<(), PlayError> {
        match self
            .dispatcher
            .exec_host(HostCmd::UpdateOutputRoute { sample_rate })?
        {
            HostReply::Ok => Ok(()),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for route update".into(),
            )),
        }
    }

    /// Reads the current output-rate observation without exposing the lower
    /// session handle.
    ///
    /// # Errors
    /// Returns an error when the canonical session cannot answer the query.
    pub fn sample_rate(&self) -> Result<SessionSampleRate, PlayError> {
        match self.dispatcher.exec(Cmd::QuerySampleRate)? {
            Reply::SampleRate(sample_rate) => Ok(sample_rate),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected host reply for sample-rate query".into(),
            )),
        }
    }

    fn session_root(sample_rate: NonZeroU32) -> Result<SessionRoot, PlayError> {
        let grid_id = BeatGridId::allocate().map_err(SessionError::from)?;
        let group = GroupState::unavailable(
            grid_id,
            sample_rate,
            SessionEpoch::new(0),
            SyncMemberKind::Group,
        );
        let view = RootView::new(&group, sample_rate);
        Ok(SessionRoot {
            sample_rate,
            group,
            view,
            id: grid_id,
        })
    }

    /// Change the canonical session tempo at the next render boundary.
    ///
    /// # Errors
    /// Returns an error when the Host rejects or cannot dispatch the update.
    pub fn set_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::SetSessionTempo { tempo })
    }

    /// Read the canonical session transport state.
    ///
    /// # Errors
    /// Returns an error when the Host cannot answer the query.
    pub fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
        match self.dispatcher.exec(Cmd::QuerySessionTransport)? {
            Reply::SessionTransport(snapshot) => Ok(snapshot),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected host reply for transport query".into(),
            )),
        }
    }

    fn validate_removal<P>(&self, player: &HostOwned<P>) -> Result<(), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
        S: Send + Sync + 'static,
    {
        if player.host_id != self.id {
            return Err(PlayError::ForeignSession);
        }
        let topology = self.topology().map_err(SessionError::from)?;
        if topology
            .members()
            .iter()
            .any(|member| member.grid().id() == player.id())
        {
            return Ok(());
        }
        Err(SessionError::from(SyncError::MemberNotFound {
            group_id: self.id,
            member_id: player.id(),
        })
        .into())
    }
}

impl<S> Host<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    /// Creates one Host with its configured realtime or offline session.
    ///
    /// # Errors
    /// Returns an error when the session root or selected runtime cannot start.
    pub fn new(config: HostConfig<S>) -> Result<Self, PlayError> {
        match config {
            HostConfig::Realtime {
                sample_rate_hint,
                output_block_frames,
                ..
            } => {
                let root = Self::session_root(sample_rate_hint)?;
                let (dispatcher, platform) = Platform::realtime(
                    root.group,
                    root.view.clone(),
                    root.sample_rate,
                    output_block_frames,
                )
                .resolve()?;
                Ok(Self::owner(
                    root.id,
                    root.view,
                    dispatcher,
                    SessionRuntime::realtime(platform),
                ))
            }
            #[cfg(feature = "offline")]
            config @ HostConfig::Offline { .. } => {
                let platform = Platform::offline().resolve()?;
                let root = Self::session_root(config.sample_rate())?;
                let (dispatcher, runtime) =
                    OfflineRuntime::new(config, root.group, root.view.clone())?;
                Ok(Self::owner(
                    root.id,
                    root.view,
                    dispatcher,
                    SessionRuntime::offline(platform, runtime),
                ))
            }
        }
    }
}

impl<S> Drop for Host<S> {
    fn drop(&mut self) {
        Platform::close(self.session.platform_mut(), self.id);
        if self.owns_session
            && let Err(error) = self.dispatcher.exec_host(HostCmd::Shutdown)
        {
            tracing::warn!(error = %PlayError::from(error), "host session shutdown failed");
        }
    }
}

impl<S: Send + Sync + 'static> BeatGrid for Host<S> {
    fn id(&self) -> BeatGridId {
        self.id
    }

    fn snapshot(&self) -> kithara_warp::BeatGridSnapshot {
        self.root_view.grid()
    }
}

impl<S: Send + Sync + 'static> SyncGroup for Host<S> {
    type NestedGroup = PlayerMember;

    fn transact(
        &mut self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        Platform::transact(self.session.platform(), &self.dispatcher, operation)
    }

    delegate::delegate! {
        to self.root_view {
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
        }
        to self.dispatcher {
            fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }
}

fn require_topology_change(result: Result<SyncAdmission, PlayError>) -> Result<(), PlayError> {
    match result {
        Ok(SyncAdmission::TopologyChanged { .. }) => Ok(()),
        Ok(_) => Err(PlayError::Internal(
            "host topology operation did not change topology".into(),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use kithara_bufpool::testing::TestPools;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn realtime_config_preserves_output_block_default_and_allows_override() {
        let default = HostConfig::<TestPools>::builder().build();
        let HostConfig::Realtime {
            output_block_frames,
            ..
        } = default
        else {
            panic!("default Host config must be realtime");
        };
        assert_eq!(output_block_frames, None);

        let frames = NonZeroU32::new(128).expect("test block size is non-zero");
        let configured = HostConfig::<TestPools>::builder()
            .output_block_frames(frames)
            .build();
        let HostConfig::Realtime {
            output_block_frames,
            ..
        } = configured
        else {
            panic!("realtime builder must create realtime config");
        };
        assert_eq!(output_block_frames, Some(frames));
    }

    #[kithara::test]
    fn host_root_owns_the_configured_sample_rate() {
        let sample_rate = NonZeroU32::new(48_000).expect("test sample rate is non-zero");
        let config = HostConfig::<TestPools>::builder()
            .sample_rate_hint(sample_rate)
            .build();
        let root = Host::<TestPools>::session_root(config.sample_rate()).expect("host root");

        assert_eq!(root.sample_rate, sample_rate);
        assert_eq!(root.view.grid().axis().sample_rate(), sample_rate);
    }
}
