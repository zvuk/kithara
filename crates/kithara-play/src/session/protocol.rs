mod wire {
    use std::{fmt, num::NonZeroU64};

    use firewheel::FirewheelCtx;
    use kithara_audio::EqBandConfig;
    use kithara_bufpool::PcmPool;
    use kithara_events::EventBus;
    use kithara_platform::sync::mpsc;

    use crate::{
        api::{
            SessionBeat, SessionDuckingMode, SessionTransportSnapshot, SlotId, Tempo,
            TransportRevision,
        },
        bridge::SlotControl,
        player::node::StreamShape,
    };

    /// Physical player registration identity owned by one session.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    #[repr(transparent)]
    pub struct PlayerId(NonZeroU64);

    impl PlayerId {
        pub(crate) const FIRST: Self = Self(NonZeroU64::MIN);

        pub(crate) fn checked_next(self) -> Option<Self> {
            self.0
                .get()
                .checked_add(1)
                .and_then(NonZeroU64::new)
                .map(Self)
        }

        #[must_use]
        pub const fn get(self) -> u64 {
            self.0.get()
        }
    }

    impl fmt::Display for PlayerId {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.get().fmt(formatter)
        }
    }

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(crate) struct SessionRosterRevision(u64);

    impl SessionRosterRevision {
        pub(crate) fn checked_next(self) -> Option<Self> {
            self.0.checked_add(1).map(Self)
        }

        #[cfg(test)]
        pub(crate) const fn new_for_test(value: u64) -> Self {
            Self(value)
        }
    }

    pub type StartStreamFn<B> = fn(&mut FirewheelCtx<B>, u32) -> Result<(), String>;

    /// One atomic validation context for bound-track preparation and activation.
    #[derive(Clone, Copy, Debug, PartialEq)]
    #[non_exhaustive]
    pub struct PreparationContext {
        roster_revision: SessionRosterRevision,
        shape: StreamShape,
        tempo: Tempo,
        transport_revision: TransportRevision,
    }

    impl PreparationContext {
        pub(crate) const fn new(
            shape: StreamShape,
            tempo: Tempo,
            transport_revision: TransportRevision,
            roster_revision: SessionRosterRevision,
        ) -> Self {
            Self {
                roster_revision,
                shape,
                tempo,
                transport_revision,
            }
        }

        pub(crate) const fn roster_revision(self) -> SessionRosterRevision {
            self.roster_revision
        }

        pub(crate) const fn shape(self) -> StreamShape {
            self.shape
        }

        pub(crate) const fn tempo(self) -> Tempo {
            self.tempo
        }

        pub(crate) const fn transport_revision(self) -> TransportRevision {
            self.transport_revision
        }
    }

    #[derive(Debug, Clone, thiserror::Error)]
    #[non_exhaustive]
    pub enum SessionError {
        #[error("player not found: {0}")]
        PlayerNotFound(PlayerId),
        #[error("player already started: {0}")]
        AlreadyStarted(PlayerId),
        #[error("player not running: {0}")]
        NotRunning(PlayerId),
        #[error("session player identity is exhausted")]
        PlayerIdExhausted,
        #[error("slot not found: {0:?}")]
        SlotNotFound(SlotId),
        #[error("session context not initialised")]
        NoContext,
        /// The session transport rejected an update.
        #[error("session transport update failed: {0}")]
        TransportSync(String),
        /// The active graph has not processed the session transport yet.
        #[error("session transport has not been processed")]
        TransportNotProcessed,
        /// A bound track cannot be prepared before the session has a tempo.
        #[error("session transport has not been configured")]
        TransportNotConfigured,
        /// A different session transport commit is awaiting a render result.
        #[error("session transport commit {revision} is still pending")]
        TransportCommitPending { revision: TransportRevision },
        /// The render graph rejected a session transport commit.
        #[error("session transport commit {revision} was rejected at the render boundary")]
        TransportCommitRejected { revision: TransportRevision },
        /// The monotonic session transport revision cannot advance further.
        #[error("session transport revision is exhausted")]
        TransportRevisionExhausted,
        /// The monotonic physical-roster revision cannot advance further.
        #[error("session physical roster revision is exhausted")]
        SessionRosterRevisionExhausted,
        /// The graph frame used to schedule a transport commit cannot advance further.
        #[error("session transport frame is exhausted")]
        TransportFrameExhausted,
        /// The active graph roster changed after tempo preparation.
        #[error("session physical roster changed after transport preparation")]
        TransportRosterChanged,
        /// The prepared participant set does not match the active graph roster.
        #[error("session tempo player roster changed: expected {expected} players, found {actual}")]
        TransportPlayersChanged { expected: usize, actual: usize },
        /// The observed transport changed after tempo preparation.
        #[error("session tempo revision changed: expected revision {expected}, found {actual}")]
        TransportRevisionChanged {
            expected: TransportRevision,
            actual: TransportRevision,
        },
        /// The active stream dimensions changed after tempo preparation.
        #[error("session stream shape changed after tempo preparation")]
        TransportStreamShapeChanged,
        #[error("eq band out of range: {band} (bands: {bands})")]
        EqBandOutOfRange { band: usize, bands: usize },
        #[error("stream start failed: {0}")]
        StreamStart(String),
        #[error("graph edit failed: {0}")]
        Graph(String),
        #[error("stream stopped: {reason}; restart failed: {source}")]
        RestartFailed { reason: String, r#source: String },
    }

    #[non_exhaustive]
    pub enum Cmd {
        RegisterPlayer {
            bus: EventBus,
            eq_layout: Vec<EqBandConfig>,
            pcm_pool: PcmPool,
        },
        UnregisterPlayer {
            player_id: PlayerId,
        },
        StartPlayer {
            master_volume: f32,
            player_id: PlayerId,
            sample_rate: u32,
        },
        StopPlayer {
            player_id: PlayerId,
        },
        AllocateSlot {
            player_id: PlayerId,
        },
        ReleaseSlot {
            player_id: PlayerId,
            slot: SlotId,
        },
        SetPlayerMasterVolume {
            player_id: PlayerId,
            volume: f32,
        },
        SetPlayerSlotVolume {
            player_id: PlayerId,
            slot: SlotId,
            volume: f32,
        },
        SetPlayerEqGain {
            band: usize,
            gain_db: f32,
            player_id: PlayerId,
        },
        SetSessionDucking {
            mode: SessionDuckingMode,
        },
        SessionDucking,
        /// Change the tempo of the shared session transport.
        SetSessionTempo {
            tempo: Tempo,
        },
        /// Commit a prepared tempo only while its session context and player roster remain current.
        SetSessionTempoChecked {
            tempo: Tempo,
            expected_context: PreparationContext,
            player_ids: Vec<PlayerId>,
        },
        /// Commit a prepared relocation while its session context and player roster remain current.
        SeekSessionChecked {
            target: SessionBeat,
            expected_context: PreparationContext,
            player_ids: Vec<PlayerId>,
        },
        /// Query the transport state last processed by the audio graph.
        SessionTransport,
        /// Query one canonical transport, stream, and physical-roster context.
        PreparationContext,
        InvalidateAudioRoute {
            reason: String,
        },
        QuerySampleRate,
        /// Query the active stream dimensions.
        QueryStreamShape,
        Tick,
    }

    pub struct CmdMsg {
        pub cmd: Cmd,
        pub reply_tx: mpsc::Sender<Reply>,
    }

    #[non_exhaustive]
    pub enum Reply {
        Ok,
        PlayerRegistered(PlayerId),
        SessionDucking(SessionDuckingMode),
        /// The transport state last processed by the audio graph.
        SessionTransport(SessionTransportSnapshot),
        /// Canonical preparation values captured by one session command.
        PreparationContext(PreparationContext),
        SlotAllocated(AllocatedSlot),
        SampleRate(u32),
        /// Active stream dimensions.
        StreamShape(StreamShape),
        Err(SessionError),
    }

    #[non_exhaustive]
    pub struct AllocatedSlot {
        pub control: SlotControl,
        pub slot: SlotId,
    }
}

mod handle {
    use kithara_audio::EqBandConfig;
    use kithara_bufpool::PcmPool;
    use kithara_events::EventBus;
    use kithara_platform::sync::Arc;

    use super::wire::{AllocatedSlot, Cmd, PlayerId, PreparationContext, Reply};
    use crate::{
        api::{SessionBeat, SessionTransportSnapshot, SlotId, Tempo},
        error::PlayError,
        player::node::StreamShape,
    };

    pub trait SessionDispatcher: Send + Sync + 'static {
        fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError>;

        fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            match self.exec(cmd)? {
                Reply::Err(err) => Err(PlayError::Session(err)),
                reply => Ok(reply),
            }
        }
    }

    #[derive(Clone)]
    pub struct SessionHandle(Arc<dyn SessionDispatcher>);

    impl SessionHandle {
        #[must_use]
        pub fn new(dispatcher: Arc<dyn SessionDispatcher>) -> Self {
            Self(dispatcher)
        }

        pub fn allocate_slot(&self, player_id: PlayerId) -> Result<AllocatedSlot, PlayError> {
            match self.exec_ok(Cmd::AllocateSlot { player_id })? {
                Reply::SlotAllocated(allocated) => Ok(allocated),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session allocate slot".into(),
                )),
            }
        }

        #[must_use]
        pub fn dispatcher(&self) -> Arc<dyn SessionDispatcher> {
            Arc::clone(&self.0)
        }

        delegate::delegate! {
            to self.0 {
                pub fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError>;
                pub fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError>;
            }
        }

        fn exec_unit(&self, cmd: Cmd) -> Result<(), PlayError> {
            match self.exec_ok(cmd)? {
                Reply::Ok => Ok(()),
                _ => Err(PlayError::Internal(
                    "unexpected reply for unit session command".into(),
                )),
            }
        }

        pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
            self.exec_unit(Cmd::InvalidateAudioRoute {
                reason: reason.to_owned(),
            })
        }

        pub(crate) fn preparation_context(&self) -> Result<PreparationContext, PlayError> {
            match self.exec_ok(Cmd::PreparationContext)? {
                Reply::PreparationContext(context) => Ok(context),
                _ => Err(PlayError::Internal(
                    "unexpected reply for preparation context query".into(),
                )),
            }
        }

        #[must_use]
        pub fn query_sample_rate(&self, fallback: u32) -> u32 {
            match self.exec(Cmd::QuerySampleRate) {
                Ok(Reply::SampleRate(sr)) => sr,
                _ => fallback,
            }
        }

        pub(crate) fn query_stream_shape(&self) -> Result<StreamShape, PlayError> {
            match self.exec_ok(Cmd::QueryStreamShape)? {
                Reply::StreamShape(shape) => Ok(shape),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session stream-shape query".into(),
                )),
            }
        }

        pub fn register_player(
            &self,
            bus: EventBus,
            eq_layout: Vec<EqBandConfig>,
            pcm_pool: PcmPool,
        ) -> Result<PlayerId, PlayError> {
            match self.exec_ok(Cmd::RegisterPlayer {
                bus,
                eq_layout,
                pcm_pool,
            })? {
                Reply::PlayerRegistered(id) => Ok(id),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session player registration".into(),
                )),
            }
        }

        pub fn release_slot(&self, player_id: PlayerId, slot: SlotId) -> Result<(), PlayError> {
            self.exec_unit(Cmd::ReleaseSlot { player_id, slot })
        }

        pub(crate) fn seek_session_checked(
            &self,
            target: SessionBeat,
            expected_context: PreparationContext,
            player_ids: Vec<PlayerId>,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SeekSessionChecked {
                target,
                expected_context,
                player_ids,
            })
        }

        pub(crate) fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
            match self.exec_ok(Cmd::SessionTransport)? {
                Reply::SessionTransport(snapshot) => Ok(snapshot),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session transport query".into(),
                )),
            }
        }

        pub fn set_player_eq_gain(
            &self,
            player_id: PlayerId,
            band: usize,
            gain_db: f32,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SetPlayerEqGain {
                band,
                gain_db,
                player_id,
            })
        }

        pub fn set_player_master_volume(
            &self,
            player_id: PlayerId,
            volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SetPlayerMasterVolume { player_id, volume })
        }

        pub fn set_player_slot_volume(
            &self,
            player_id: PlayerId,
            slot: SlotId,
            volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SetPlayerSlotVolume {
                player_id,
                slot,
                volume,
            })
        }

        pub(crate) fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SetSessionTempo { tempo })
        }

        pub(crate) fn set_session_tempo_checked(
            &self,
            tempo: Tempo,
            expected_context: PreparationContext,
            player_ids: Vec<PlayerId>,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::SetSessionTempoChecked {
                tempo,
                expected_context,
                player_ids,
            })
        }

        pub(crate) fn shares_dispatcher(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }

        pub fn start_player(
            &self,
            player_id: PlayerId,
            sample_rate: u32,
            master_volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_unit(Cmd::StartPlayer {
                master_volume,
                player_id,
                sample_rate,
            })
        }

        pub fn stop_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
            self.exec_unit(Cmd::StopPlayer { player_id })
        }

        pub fn tick(&self) -> Result<(), PlayError> {
            self.exec_unit(Cmd::Tick)
        }

        pub fn unregister_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
            self.exec_unit(Cmd::UnregisterPlayer { player_id })
        }
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::*;

        struct WrongReplyDispatcher;

        impl SessionDispatcher for WrongReplyDispatcher {
            fn exec(&self, _cmd: Cmd) -> Result<Reply, PlayError> {
                Ok(Reply::SampleRate(48_000))
            }
        }

        #[kithara::test]
        fn unit_command_rejects_a_non_unit_reply() {
            let handle = SessionHandle::new(Arc::new(WrongReplyDispatcher));

            assert!(matches!(
                handle.stop_player(PlayerId::FIRST),
                Err(PlayError::Internal(message))
                    if message == "unexpected reply for unit session command"
            ));
        }
    }
}

pub use handle::{SessionDispatcher, SessionHandle};
pub(crate) use wire::SessionRosterRevision;
pub use wire::{
    AllocatedSlot, Cmd, CmdMsg, PlayerId, PreparationContext, Reply, SessionError, StartStreamFn,
};
