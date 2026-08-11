mod wire {
    use firewheel::FirewheelCtx;
    use kithara_audio::{CoordinateError, EqBandConfig, SessionAnchorCell, SessionBeat};
    use kithara_bufpool::PcmPool;
    use kithara_events::EventBus;
    use kithara_platform::sync::{Arc, mpsc};

    use crate::{
        api::{
            SessionDuckingMode, SessionTransportSnapshot, SlotId, SyncUnavailable, Tempo,
            TrackBinding,
        },
        bridge::{MixTapWriter, SlotControl},
    };

    pub type PlayerId = u64;

    pub type StartStreamFn<B> =
        Box<dyn FnMut(&mut FirewheelCtx<B>, u32) -> Result<(), String> + Send>;

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
        #[error("eq band out of range: {band} (bands: {bands})")]
        EqBandOutOfRange { band: usize, bands: usize },
        #[error("master volume {level} out of range for player {player_id}")]
        MasterVolumeOutOfRange { player_id: PlayerId, level: f32 },
        #[error("duplicate player in master volume batch: {0}")]
        DuplicatePlayer(PlayerId),
        #[error("stream start failed: {0}")]
        StreamStart(String),
        #[error("graph edit failed: {0}")]
        Graph(String),
        #[error("session mix tap already has a consumer")]
        MixTapActive,
        #[error("session transport has not been processed")]
        TransportNotProcessed,
        #[error("session transport commit was rejected at the render boundary")]
        TransportCommitRejected,
        #[error("session transport update failed: {0}")]
        TransportSync(String),
        #[error("session transport frame is exhausted")]
        TransportFrameExhausted,
        #[error("session transport revision is exhausted")]
        TransportRevisionExhausted,
        #[error("bound player {player_id} has no compiled exact-span engine")]
        BoundEngineUnavailable { player_id: PlayerId },
        #[error(
            "session tempo {beats_per_minute} BPM asks bound player {player_id} for unsupported elastic source rate {source_frames_per_output}"
        )]
        BoundTempoOutsideEnvelope {
            player_id: PlayerId,
            beats_per_minute: f64,
            source_frames_per_output: f64,
        },
        #[error(
            "bound player {player_id} cannot resolve the committed session span {start_beat}..{end_beat}"
        )]
        BoundSpanOutsideMap {
            player_id: PlayerId,
            start_beat: f64,
            end_beat: f64,
        },
        #[error("bound player {player_id} has an invalid session span")]
        BoundSpanCoordinate {
            player_id: PlayerId,
            #[source]
            reason: CoordinateError,
        },
        #[error("bound player {player_id} has an unusable track binding")]
        BoundBindingUnavailable {
            player_id: PlayerId,
            #[source]
            reason: SyncUnavailable,
        },
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
        BindPlayer {
            player_id: PlayerId,
            binding: TrackBinding,
            at: SessionBeat,
        },
        UnbindPlayer {
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
        SetPlayerMasterVolumes {
            levels: Vec<PlayerLevel>,
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
        SetPlayerEqLayout {
            eq_layout: Vec<EqBandConfig>,
            player_id: PlayerId,
        },
        EnableMixTap {
            writer: MixTapWriter,
        },
        DisableMixTap,
        SetSessionDucking {
            mode: SessionDuckingMode,
        },
        SessionDucking,
        SetSessionTempo {
            tempo: Tempo,
        },
        SetSessionPlaying {
            playing: bool,
        },
        SeekSession {
            target: SessionBeat,
        },
        QuerySessionTransport,
        QuerySessionAnchor,
        InvalidateAudioRoute {
            reason: String,
        },
        QuerySampleRate,
        Tick,
    }

    pub struct CmdMsg {
        pub cmd: Cmd,
        pub reply_tx: mpsc::Sender<Reply>,
    }

    /// One player's session-input level in a batch update. `level` is a linear
    /// amplitude in `0.0..=1.0`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    #[non_exhaustive]
    pub struct PlayerLevel {
        pub player_id: PlayerId,
        pub level: f32,
    }

    impl PlayerLevel {
        #[must_use]
        pub fn new(player_id: PlayerId, level: f32) -> Self {
            Self { player_id, level }
        }
    }

    #[non_exhaustive]
    pub enum Reply {
        Ok,
        PlayerRegistered(PlayerId),
        SessionDucking(SessionDuckingMode),
        SessionTransport(SessionTransportSnapshot),
        SessionAnchor(Arc<SessionAnchorCell>),
        SlotAllocated(AllocatedSlot),
        SampleRate(SessionSampleRate),
        Err(SessionError),
    }

    /// What the session knows about its output rate.
    #[derive(Clone, Copy)]
    #[non_exhaustive]
    pub struct SessionSampleRate {
        /// The rate the output stream runs at, once a stream exists.
        pub measured: Option<u32>,
        /// The rate the session last asked the device for.
        pub requested: u32,
    }

    impl SessionSampleRate {
        #[must_use]
        pub const fn new(measured: Option<u32>, requested: u32) -> Self {
            Self {
                measured,
                requested,
            }
        }

        /// The rate to build a resampler for.
        #[must_use]
        pub const fn output(self) -> u32 {
            match self.measured {
                Some(measured) => measured,
                None => self.requested,
            }
        }
    }

    #[non_exhaustive]
    pub struct AllocatedSlot {
        pub control: SlotControl,
        pub slot: SlotId,
    }
}

mod handle {
    use kithara_audio::{EqBandConfig, SessionAnchorCell, SessionBeat};
    use kithara_bufpool::PcmPool;
    use kithara_events::EventBus;
    use kithara_platform::sync::Arc;

    use super::wire::{
        AllocatedSlot, Cmd, PlayerId, PlayerLevel, Reply, SessionError, SessionSampleRate,
    };
    use crate::{
        api::{SessionTransportSnapshot, SlotId, Tempo, TrackBinding},
        error::PlayError,
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

        /// The committed session transport as the audio graph last processed it.
        ///
        /// # Errors
        ///
        /// Returns [`PlayError`] when the session rejects the query or the
        /// transport has not been processed yet.
        pub fn transport(&self) -> Result<SessionTransportSnapshot, PlayError> {
            match self.exec_ok(Cmd::QuerySessionTransport)? {
                Reply::SessionTransport(snapshot) => Ok(snapshot),
                _ => Err(PlayError::Session(SessionError::TransportNotProcessed)),
            }
        }

        /// Commits a session tempo, in force from the next block boundary.
        ///
        /// The grid every bound deck follows. This is the session's own tempo,
        /// not a deck's playback speed: a deck's speed knob moves that deck,
        /// this moves what "on the beat" means for all of them.
        ///
        /// # Errors
        ///
        /// Returns [`PlayError`] when the session rejects the commit — a
        /// commit is already in flight, or the transport is not installed.
        pub fn set_session_tempo(&self, tempo: Tempo) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetSessionTempo { tempo }).map(drop)
        }

        /// Starts or stops the session clock.
        ///
        /// A stopped transport holds its beat, so a deck armed on a coming
        /// beat waits rather than drifting.
        ///
        /// # Errors
        ///
        /// Returns [`PlayError`] when the session rejects the commit.
        pub fn set_session_playing(&self, playing: bool) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetSessionPlaying { playing }).map(drop)
        }

        /// The session grid a deck binds to, shared so a tempo commit reaches
        /// every bound deck without any of them holding a copy.
        ///
        /// # Errors
        ///
        /// Returns [`PlayError`] when the session has no transport installed.
        pub fn anchor(&self) -> Result<Arc<SessionAnchorCell>, PlayError> {
            match self.exec_ok(Cmd::QuerySessionAnchor)? {
                Reply::SessionAnchor(anchor) => Ok(anchor),
                _ => Err(PlayError::Session(SessionError::TransportNotProcessed)),
            }
        }

        pub(crate) fn bind_player(
            &self,
            player_id: PlayerId,
            binding: TrackBinding,
            at: SessionBeat,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::BindPlayer {
                player_id,
                binding,
                at,
            })
            .map(drop)
        }

        pub(crate) fn unbind_player(&self, player_id: PlayerId) -> Result<(), PlayError> {
            self.exec_ok(Cmd::UnbindPlayer { player_id }).map(drop)
        }

        pub fn invalidate_audio_route(&self, reason: &str) -> Result<(), PlayError> {
            self.exec_ok(Cmd::InvalidateAudioRoute {
                reason: reason.to_owned(),
            })
            .map(|_| ())
        }

        pub fn sample_rate(&self) -> Result<SessionSampleRate, PlayError> {
            match self.exec_ok(Cmd::QuerySampleRate)? {
                Reply::SampleRate(sample_rate) => Ok(sample_rate),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session sample rate query".into(),
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

        pub fn set_player_master_volumes(&self, levels: Vec<PlayerLevel>) -> Result<(), PlayError> {
            if levels.is_empty() {
                return Ok(());
            }
            self.exec_ok(Cmd::SetPlayerMasterVolumes { levels })
                .map(|_| ())
        }

        pub fn set_player_eq_layout(
            &self,
            player_id: PlayerId,
            eq_layout: Vec<EqBandConfig>,
        ) -> Result<(), PlayError> {
            self.exec_ok(Cmd::SetPlayerEqLayout {
                eq_layout,
                player_id,
            })
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
    AllocatedSlot, Cmd, CmdMsg, PlayerId, PlayerLevel, Reply, SessionError, SessionSampleRate,
    StartStreamFn,
};
