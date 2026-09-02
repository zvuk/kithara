use std::{marker::PhantomData, num::NonZeroU32, ops::Deref};

use bon::Builder;
use kithara_platform::sync::Arc;
use kithara_play::{
    GroupState, PlayError, SessionBinding, SessionDispatcher,
    player::{PlayerControlSource, PlayerMember},
};
use kithara_warp::{
    BeatGrid, BeatGridId, SessionEpoch, SyncAdmission, SyncApplied, SyncError, SyncGroup,
    SyncGroupSnapshot, SyncMember, SyncMemberKind, SyncOperation, SyncRejected, SyncStatusSnapshot,
    TopologyOperation,
};

mod platform;

use platform::Platform;

#[cfg(any(test, feature = "probe"))]
use crate::api::SessionDuckingMode;
use crate::{
    api::HostLevel,
    bridge::MixTapWriter,
    session::{
        Cmd, HostCmd, HostDispatcher, HostReply, Reply, RootView, SessionError, SessionSampleRate,
        StreamShape,
    },
};

struct Defaults {
    output_block_frames: NonZeroU32,
    sample_rate: NonZeroU32,
}

const DEFAULTS: Defaults = Defaults {
    output_block_frames: match NonZeroU32::new(128) {
        Some(frames) => frames,
        None => unreachable!(),
    },
    sample_rate: match NonZeroU32::new(44_100) {
        Some(sample_rate) => sample_rate,
        None => unreachable!(),
    },
};

/// Configuration for the shared output session owned by [`Host`].
#[derive(Clone, Copy, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct HostConfig {
    /// Initial device-rate hint. Physical route changes may update it later.
    #[builder(default = DEFAULTS.sample_rate)]
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    /// Desired CPAL output callback size in frames. The backend may clamp or ignore it.
    #[builder(default = DEFAULTS.output_block_frames)]
    #[field(get, copy)]
    output_block_frames: NonZeroU32,
}

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
    id: BeatGridId,
    owns_session: bool,
    root_view: RootView,
    dispatcher: Arc<dyn HostDispatcher<S>>,
    platform: Platform<S>,
}

struct SessionRoot {
    id: BeatGridId,
    requested_shape: StreamShape,
    group: GroupState<PlayerMember>,
    view: RootView,
}

impl<S> Host<S> {
    fn session_root(config: HostConfig) -> Result<SessionRoot, PlayError> {
        let grid_id = BeatGridId::allocate().map_err(SessionError::from)?;
        let sample_rate = config.sample_rate;
        let requested_shape = StreamShape::new(config.output_block_frames, sample_rate);
        let group = GroupState::unavailable(
            grid_id,
            sample_rate,
            SessionEpoch::new(0),
            SyncMemberKind::Group,
        );
        let view = RootView::new(&group);
        Ok(SessionRoot {
            id: grid_id,
            requested_shape,
            group,
            view,
        })
    }

    fn owner(
        id: BeatGridId,
        root_view: RootView,
        dispatcher: Arc<dyn HostDispatcher<S>>,
        platform: Platform<S>,
    ) -> Self {
        Self {
            id,
            owns_session: true,
            root_view,
            dispatcher,
            platform,
        }
    }

    fn bind_player<P>(&self, player: &mut P) -> Result<(BeatGridId, P::Control), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        let grid_id = player.id();
        let dispatcher: Arc<dyn SessionDispatcher<S>> = self.dispatcher.clone();
        player.attach_session(SessionBinding::new(dispatcher))?;
        Ok((grid_id, player.control()))
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

    fn owned<P>(&self, id: BeatGridId, control: P::Control) -> HostOwned<P>
    where
        P: PlayerControlSource,
    {
        HostOwned {
            host_id: self.id,
            id,
            control,
            marker: PhantomData,
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

    fn detach_member(&self, member: BeatGridId) -> Result<(), PlayError> {
        let operations = Box::new([TopologyOperation::Detach { member }]);
        require_topology_change(self.dispatcher.transact_current(operations))
    }

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

    /// Installs the single post-limiter mix tap.
    ///
    /// # Errors
    /// Returns an error when a tap is active or graph dispatch fails.
    pub fn enable_mix_tap(&self, writer: MixTapWriter) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::EnableMixTap { writer })
    }

    /// Removes the post-limiter mix tap.
    ///
    /// # Errors
    /// Returns an error when graph dispatch fails.
    pub fn disable_mix_tap(&self) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::DisableMixTap)
    }

    /// Updates the shared output-session ducking mode.
    ///
    /// # Errors
    /// Returns an error when the canonical session rejects the update.
    #[cfg(any(test, feature = "probe"))]
    pub(crate) fn set_ducking_mode(&self, mode: SessionDuckingMode) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::SetSessionDucking { mode })
    }

    /// Reads the shared output-session ducking mode.
    ///
    /// # Errors
    /// Returns an error when the canonical session cannot answer the query.
    #[cfg(any(test, feature = "probe"))]
    pub(crate) fn ducking_mode(&self) -> Result<SessionDuckingMode, PlayError> {
        match self.dispatcher.exec(Cmd::SessionDucking)? {
            Reply::SessionDucking(mode) => Ok(mode),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected host reply for ducking query".into(),
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
}

impl<S> Drop for Host<S> {
    fn drop(&mut self) {
        Platform::close(&mut self.platform, self.id);
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

    delegate::delegate! {
        to self.root_view {
            fn topology(&self) -> Result<SyncGroupSnapshot, SyncError>;
            fn status(&self) -> SyncStatusSnapshot;
        }
        to self.dispatcher {
            fn acknowledge(&mut self, applied: SyncApplied) -> Result<SyncStatusSnapshot, SyncError>;
        }
    }

    fn transact(
        &mut self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        Platform::transact(&self.dispatcher, operation)
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
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn host_config_output_block_frames_default_and_override() {
        let default = HostConfig::builder().build();
        assert_eq!(default.output_block_frames(), DEFAULTS.output_block_frames);

        let frames = NonZeroU32::new(256).expect("test block size is non-zero");
        let configured = HostConfig::builder().output_block_frames(frames).build();
        assert_eq!(configured.output_block_frames(), frames);
    }
}
