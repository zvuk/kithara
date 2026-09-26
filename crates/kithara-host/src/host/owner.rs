use std::{marker::PhantomData, num::NonZeroU32, ops::Deref};

use kithara_bufpool::HasPool;
use kithara_output::OutputGroup;
use kithara_platform::sync::Arc;
use kithara_play::{
    PlayError, SessionBinding, SessionDispatcher, SessionTransportSnapshot, Tempo,
    player::PlayerControlSource,
};
use kithara_signal::SessionEpoch;
use kithara_sync::{
    GroupState, ParentFact, SyncAdmission, SyncAttachment, SyncError, SyncGroup, SyncGroupSnapshot,
    SyncIntent, SyncMember, SyncMemberKind, SyncMode, SyncOperation, SyncReceipt, SyncRejected,
    SyncStaged, SyncStatusSnapshot, SyncTransition, TopologyOperation,
};
use kithara_warp::{BeatGrid, BeatGridId};

#[cfg(feature = "offline")]
use super::offline::OfflineRuntime;
use super::{
    HostConfig,
    platform::{Platform, PlatformResult},
};
use crate::{
    PlayerMember,
    api::{DeckSyncState, HostLevel},
    session::{
        Cmd, HostCmd, HostDispatcher, HostReply, Reply, RootView, SessionError, SessionSampleRate,
        protocol::SyncCmd,
    },
};

/// Typed command proxy for one player value exclusively resident in a Host.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct HostOwned<P: PlayerControlSource> {
    host_id: BeatGridId,
    #[field(get, copy)]
    id: BeatGridId,
    #[field(get, copy, vis = "pub(super)")]
    track_id: BeatGridId,
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
    pub(super) dispatcher: Arc<dyn HostDispatcher<S>>,
    pub(super) id: BeatGridId,
    pub(super) root_view: RootView,
    pub(super) session: SessionRuntime<S>,
    pub(super) owns_session: bool,
}

pub(super) enum SessionRuntime<S> {
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
    pub(super) const fn offline_runtime_mut(&mut self) -> Option<&mut OfflineRuntime<S>> {
        match self {
            Self::Offline { runtime, .. } => Some(runtime),
            Self::Realtime(_) => None,
        }
    }

    pub(super) const fn platform(&self) -> &Platform<S> {
        match self {
            Self::Realtime(platform) => platform,
            #[cfg(feature = "offline")]
            Self::Offline { platform, .. } => platform,
        }
    }

    pub(super) const fn platform_mut(&mut self) -> &mut Platform<S> {
        match self {
            Self::Realtime(platform) => platform,
            #[cfg(feature = "offline")]
            Self::Offline { platform, .. } => platform,
        }
    }

    pub(super) const fn realtime(platform: Platform<S>) -> Self {
        Self::Realtime(platform)
    }
}

pub(super) struct SessionRoot {
    pub(super) id: BeatGridId,
    pub(super) group: GroupState<PlayerMember>,
    pub(super) sample_rate: NonZeroU32,
    pub(super) view: RootView,
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

    pub(super) fn attach_member(&self, member: PlayerMember) -> Result<(), PlayError> {
        let operations = Box::new([TopologyOperation::Attach {
            member: SyncMember::Group {
                alignment: None,
                group: Box::new(member),
            },
        }]);
        require_topology_change(self.dispatcher.transact_current(operations))
    }

    pub(super) fn bind_player<P>(
        &self,
        player: &mut P,
    ) -> Result<(SyncAttachment, P::Control), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        let dispatcher: Arc<dyn SessionDispatcher<S>> = self.dispatcher.clone();
        let group_id = player.sync_group_grid_id();
        let track_id = player.sync_track_grid_id();
        let gate = match self.dispatcher.exec(Cmd::RegisterSyncMember {
            group: group_id,
            member: track_id,
        })? {
            Reply::SyncGate(gate) => gate,
            Reply::Err(error) => return Err(error.into()),
            _ => {
                return Err(PlayError::Internal(
                    "unexpected sync-member registration reply".into(),
                ));
            }
        };
        let attachment = match player.attach_session(
            SessionBinding::new(dispatcher, self.requested_sample_rate()).with_sync_gate(gate),
        ) {
            Ok(attachment) => attachment,
            Err(error) => {
                self.retire_sync_member(track_id)?;
                return Err(error);
            }
        };
        Ok((attachment, player.control()))
    }

    pub(super) fn detach_member(&self, member: BeatGridId) -> Result<(), PlayError> {
        let operations = Box::new([TopologyOperation::Detach { member }]);
        require_topology_change(self.dispatcher.transact_current(operations))
    }

    pub(super) fn retire_sync_member(&self, member: BeatGridId) -> Result<(), PlayError> {
        match self.dispatcher.exec(Cmd::RetireSyncMember { member })? {
            Reply::Ok => Ok(()),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected sync-member retirement reply".into(),
            )),
        }
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

    pub(super) fn owned<P>(
        &self,
        id: BeatGridId,
        track_id: BeatGridId,
        control: P::Control,
    ) -> HostOwned<P>
    where
        P: PlayerControlSource,
    {
        HostOwned {
            id,
            track_id,
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

    /// Reads the processed Host tempo and transport, once its first commit
    /// has reached the callback.
    ///
    /// # Errors
    /// Returns an error while the transport has no processed observation or
    /// when the session cannot answer the query.
    pub fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
        match self.dispatcher.exec(Cmd::QuerySessionTransport)? {
            Reply::SessionTransport(snapshot) => Ok(snapshot),
            Reply::Err(error) => Err(error.into()),
            _ => Err(PlayError::Internal(
                "unexpected host reply for session transport query".into(),
            )),
        }
    }

    /// Reads one attached deck's accepted mode and actual executor evidence.
    ///
    /// # Errors
    /// Returns an error for a foreign or detached deck, or when the canonical
    /// session cannot answer the query.
    pub fn deck_sync_state<P>(&self, deck: &HostOwned<P>) -> Result<DeckSyncState, PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        if deck.host_id != self.id {
            return Err(PlayError::ForeignSession);
        }
        match self
            .dispatcher
            .exec_host(HostCmd::Sync(SyncCmd::QueryDeckState { target: deck.id() }))?
        {
            HostReply::DeckSyncState(snapshot) => Ok(snapshot),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for deck sync query".into(),
            )),
        }
    }

    /// Asks the Host to align one deck using its committed load and render
    /// observation. The Host selects the musical boundary on its own clock.
    ///
    /// # Errors
    /// Returns the owner's refusal when the deck is foreign, unavailable, or
    /// no longer matches its current session observation.
    pub fn request_deck_sync<P>(
        &self,
        deck: &HostOwned<P>,
        intent: SyncIntent,
    ) -> Result<(), PlayError>
    where
        P: PlayerControlSource<Schema = S>,
    {
        if deck.host_id != self.id {
            return Err(PlayError::ForeignSession);
        }
        let observation = P::resident_sync_observation(deck.control())?;
        match self
            .dispatcher
            .exec_host(HostCmd::Sync(SyncCmd::RequestDeckSync {
                target: deck.id(),
                member: deck.track_id,
                intent,
                observation,
            }))? {
            HostReply::Admission(Ok(_)) => Ok(()),
            HostReply::Admission(Err(rejected)) => {
                let (reason, _) = <(SyncError, SyncOperation<PlayerMember>)>::from(rejected);
                Err(SessionError::Sync(reason).into())
            }
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected host reply for deck sync request".into(),
            )),
        }
    }

    pub(super) fn session_root(sample_rate: NonZeroU32) -> Result<SessionRoot, PlayError> {
        let grid_id = BeatGridId::allocate().map_err(SessionError::from)?;
        let group = GroupState::unavailable(
            grid_id,
            sample_rate,
            SessionEpoch::new(0),
            SyncMemberKind::Group,
            SyncMode::Off,
        );
        let view = RootView::new(&group, sample_rate);
        Ok(SessionRoot {
            sample_rate,
            group,
            view,
            id: grid_id,
        })
    }

    /// Move the output stream to `sample_rate`, keeping Host-owned graph state.
    ///
    /// # Errors
    /// Returns an error when the session cannot restart its output at that rate.
    pub fn set_sample_rate(&self, sample_rate: NonZeroU32) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::SetSampleRate { sample_rate })
    }

    /// Change the canonical session tempo at the next render boundary.
    ///
    /// # Errors
    /// Returns an error when the Host rejects or cannot dispatch the update.
    pub fn set_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
        self.exec_play_ok(Cmd::SetSessionTempo { tempo })
    }

    pub(super) fn validate_removal<P>(&self, player: &HostOwned<P>) -> Result<(), PlayError>
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
                limiter,
                ..
            } => {
                let root = Self::session_root(sample_rate_hint)?;
                let (dispatcher, platform) = Platform::realtime(
                    root.group,
                    root.view.clone(),
                    root.sample_rate,
                    output_block_frames,
                    limiter,
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

    fn mode(&self) -> SyncMode {
        SyncMode::Off
    }

    /// The Host's session transport owns its axis and tempo; no parent fact
    /// can reach it.
    fn stage_fact(&self, _fact: ParentFact) -> Result<SyncStaged, SyncError> {
        Err(SyncError::SessionRoot { group_id: self.id })
    }

    /// Nothing is ever staged on a session root, so nothing is applied.
    fn apply_staged(&mut self, _staged: SyncStaged) -> SyncTransition {
        SyncTransition::default()
    }

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
    }

    fn acknowledge(&mut self, receipt: SyncReceipt) -> Result<SyncStatusSnapshot, SyncError> {
        match self
            .dispatcher
            .exec_host(HostCmd::Play(Cmd::AcknowledgeSync { receipt }))
        {
            Ok(HostReply::Play(Reply::SyncAcknowledged(_))) => Ok(self.root_view.status()),
            _ => Err(SyncError::OwnerUnavailable),
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
    use kithara_signal::SessionFrame;
    use kithara_sync::{ParentGridUpdate, ParentWithdrawal, SessionAxisUpdate};
    use kithara_test_utils::{bufpool::TestPools, kithara};
    use kithara_warp::{BeatGridStamp, MapAxis, SessionAnchor, SessionBeat};

    use super::*;

    #[kithara::test(native, flash(false))]
    fn a_host_is_a_session_root_and_refuses_every_parent_fact() {
        let host =
            Host::<TestPools>::new(HostConfig::builder().build()).expect("fixture realtime Host");
        let grid = host.snapshot();
        let MapAxis::Session(axis) = grid.axis() else {
            panic!("a Host grid lives on the session axis");
        };
        let axis_update = SessionAxisUpdate::new(axis);
        let segment = ParentGridUpdate::new(
            BeatGridStamp::new(
                BeatGridId::allocate().expect("parent identity"),
                grid.revision(),
            ),
            axis.epoch(),
            SessionAnchor::new(
                SessionFrame::new(0),
                SessionBeat::default(),
                2.0,
                axis.sample_rate(),
            )
            .expect("parent anchor"),
            None,
        );
        let refusal = SyncError::SessionRoot {
            group_id: host.id(),
        };

        let withdrawal = ParentWithdrawal::new(segment.parent(), SessionFrame::new(0), None);
        for fact in [
            ParentFact::Axis(axis_update),
            ParentFact::Segment(segment),
            ParentFact::Withdrawn(withdrawal),
        ] {
            assert_eq!(host.stage_fact(fact).err(), Some(refusal.clone()));
        }
        assert_eq!(host.snapshot().stamp(), grid.stamp());
    }

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
