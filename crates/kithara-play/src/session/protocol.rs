mod wire {
    use firewheel::FirewheelCtx;
    use kithara_audio::EqBandConfig;
    use kithara_bufpool::PcmPool;
    use kithara_platform::sync::mpsc;

    use crate::{
        api::{SessionDuckingMode, SessionTransportSnapshot, SlotId, Tempo},
        bridge::SlotControl,
        rt::StreamShape,
    };

    pub type PlayerId = u64;

    pub type StartStreamFn<B> = fn(&mut FirewheelCtx<B>, u32) -> Result<(), String>;

    /// Transport commit and stream dimensions captured by one session command.
    #[derive(Clone, Copy)]
    #[non_exhaustive]
    pub struct BindingPreparation {
        /// Render-observed session transport revision.
        pub revision: u64,
        /// Stream dimensions paired with the observed revision.
        pub shape: StreamShape,
        /// Session tempo paired with the observed revision.
        pub tempo: Tempo,
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
        TransportCommitPending { revision: u64 },
        /// The render graph rejected a session transport commit.
        #[error("session transport commit {revision} was rejected at the render boundary")]
        TransportCommitRejected { revision: u64 },
        /// The monotonic session transport revision cannot advance further.
        #[error("session transport revision is exhausted")]
        TransportRevisionExhausted,
        /// The graph frame used to schedule a transport commit cannot advance further.
        #[error("session transport frame is exhausted")]
        TransportFrameExhausted,
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
        /// Query the transport state last processed by the audio graph.
        SessionTransport,
        /// Query one canonical transport and stream-shape preparation snapshot.
        BindingPreparation,
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
        BindingPreparation(BindingPreparation),
        SlotAllocated(AllocatedSlot),
        SampleRate(u32),
        /// Active stream dimensions.
        StreamShape(StreamShape),
        Err(SessionError),
    }

    #[non_exhaustive]
    pub struct AllocatedSlot {
        pub slot: SlotId,
        pub control: SlotControl,
    }
}

mod handle {
    use kithara_audio::EqBandConfig;
    use kithara_bufpool::PcmPool;
    use kithara_platform::sync::Arc;

    use super::wire::{AllocatedSlot, BindingPreparation, Cmd, PlayerId, Reply};
    use crate::{
        api::{SessionTransportSnapshot, SlotId, Tempo},
        error::PlayError,
        rt::StreamShape,
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

        #[must_use]
        pub fn dispatcher(&self) -> Arc<dyn SessionDispatcher> {
            Arc::clone(&self.0)
        }

        pub fn allocate_slot(&self, player_id: PlayerId) -> Result<AllocatedSlot, PlayError> {
            match self.exec_ok(Cmd::AllocateSlot { player_id })? {
                Reply::SlotAllocated(allocated) => Ok(allocated),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session allocate slot".into(),
                )),
            }
        }

        pub fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            self.0.exec(cmd)
        }

        pub fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            self.0.exec_ok(cmd)
        }

        pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
            self.exec_ok(Cmd::InvalidateAudioRoute {
                reason: reason.to_owned(),
            })
            .map(|_| ())
        }

        #[must_use]
        pub fn query_sample_rate(&self, fallback: u32) -> u32 {
            match self.exec(Cmd::QuerySampleRate) {
                Ok(Reply::SampleRate(sr)) => sr,
                _ => fallback,
            }
        }

        pub fn register_player(
            &self,
            eq_layout: Vec<EqBandConfig>,
            pcm_pool: PcmPool,
        ) -> Result<PlayerId, PlayError> {
            match self.exec_ok(Cmd::RegisterPlayer {
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
            self.exec_ok(Cmd::ReleaseSlot { player_id, slot })
                .map(|_| ())
        }

        pub fn set_player_eq_gain(
            &self,
            player_id: PlayerId,
            band: usize,
            gain_db: f32,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetPlayerEqGain {
                band,
                gain_db,
                player_id,
            })
            .map(|_| ())
        }

        pub fn set_player_master_volume(
            &self,
            player_id: PlayerId,
            volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetPlayerMasterVolume { player_id, volume })
                .map(|_| ())
        }

        pub fn set_player_slot_volume(
            &self,
            player_id: PlayerId,
            slot: SlotId,
            volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetPlayerSlotVolume {
                player_id,
                slot,
                volume,
            })
            .map(|_| ())
        }

        pub(crate) fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetSessionTempo { tempo }).map(|_| ())
        }

        pub(crate) fn session_transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
            match self.exec_ok(Cmd::SessionTransport)? {
                Reply::SessionTransport(snapshot) => Ok(snapshot),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session transport query".into(),
                )),
            }
        }

        pub(crate) fn binding_preparation(&self) -> Result<BindingPreparation, PlayError> {
            match self.exec_ok(Cmd::BindingPreparation)? {
                Reply::BindingPreparation(preparation) => Ok(preparation),
                _ => Err(PlayError::Internal(
                    "unexpected reply for binding preparation query".into(),
                )),
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

        pub fn start_player(
            &self,
            player_id: PlayerId,
            sample_rate: u32,
            master_volume: f32,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::StartPlayer {
                master_volume,
                player_id,
                sample_rate,
            })
            .map(|_| ())
        }

        pub fn stop_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
            self.exec_ok(Cmd::StopPlayer { player_id }).map(|_| ())
        }

        pub fn tick(&self) -> Result<(), PlayError> {
            self.exec_ok(Cmd::Tick).map(|_| ())
        }

        pub fn unregister_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
            self.exec_ok(Cmd::UnregisterPlayer { player_id })
                .map(|_| ())
        }
    }
}

pub use handle::{SessionDispatcher, SessionHandle};
pub use wire::{
    AllocatedSlot, BindingPreparation, Cmd, CmdMsg, PlayerId, Reply, SessionError, StartStreamFn,
};
