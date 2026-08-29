mod wire {
    use kithara_bufpool::SamplePool;
    use kithara_events::EventBus;
    use kithara_warp::{BeatGridId, BeatGridIdAllocationError, SyncError};

    use crate::{
        api::{SessionBeat, SessionDuckingMode, SessionTransportSnapshot, SlotId, Tempo},
        bridge::{MixTapWriter, SlotControl},
        effects::eq::EqBandConfig,
    };

    pub type PlayerId = u64;

    #[derive(Debug, Clone, thiserror::Error)]
    #[non_exhaustive]
    pub enum SessionError {
        #[error("player not found: {0}")]
        PlayerNotFound(PlayerId),
        #[error("invalid session sample rate: {0}")]
        InvalidSampleRate(u32),
        #[error("player identity space is exhausted")]
        PlayerIdExhausted,
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
        #[error(transparent)]
        Sync(#[from] SyncError),
        #[error(transparent)]
        BeatGridIdAllocation(#[from] BeatGridIdAllocationError),
        #[error("stream stopped: {reason}; restart failed: {source}")]
        RestartFailed { reason: String, r#source: String },
    }

    pub enum Cmd {
        RegisterPlayer {
            grid_id: BeatGridId,
            bus: EventBus,
            eq_layout: Vec<EqBandConfig>,
            sample_pool: SamplePool,
            sample_rate: u32,
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
        #[cfg(any(test, feature = "probe"))]
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
        InvalidateAudioRoute {
            reason: String,
        },
        QuerySampleRate,
        Tick,
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
        pub const fn new(player_id: PlayerId, level: f32) -> Self {
            Self { player_id, level }
        }
    }

    #[non_exhaustive]
    pub enum Reply {
        Ok,
        PlayerRegistered(PlayerId),
        SessionDucking(SessionDuckingMode),
        SessionTransport(SessionTransportSnapshot),
        SlotAllocated(AllocatedSlot),
        SampleRate(SessionSampleRate),
        Err(SessionError),
    }

    /// What the session knows about its output rate.
    #[derive(Clone, Copy)]
    #[non_exhaustive]
    pub struct SessionSampleRate {
        /// The current Firewheel output rate; `None` means no output is measured.
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

    impl AllocatedSlot {
        #[must_use]
        pub fn new(control: SlotControl, slot: SlotId) -> Self {
            Self { control, slot }
        }
    }
}

mod handle {
    use kithara_audio::ConsumerWakeMode;
    use kithara_bufpool::SamplePool;
    use kithara_events::EventBus;
    use kithara_platform::sync::{Arc, Mutex};
    use kithara_warp::BeatGridId;

    #[cfg(any(test, feature = "probe"))]
    use super::wire::PlayerLevel;
    use super::wire::{AllocatedSlot, Cmd, PlayerId, Reply, SessionSampleRate};
    use crate::{api::SlotId, effects::eq::EqBandConfig, error::PlayError};

    pub trait SessionDispatcher: Send + Sync + 'static {
        fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError>;

        /// Describe how audio consumers hosted by this session may wake workers.
        /// Every one of them reads from the render callback, offline backends
        /// included.
        fn consumer_wake_mode(&self) -> ConsumerWakeMode;

        fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            match self.exec(cmd)? {
                Reply::Err(err) => Err(PlayError::Session(err)),
                reply => Ok(reply),
            }
        }
    }

    /// Opaque one-shot capability used to attach a Player to its session.
    ///
    /// The dispatcher is deliberately inaccessible: decorators may only pass
    /// this capability down to their resident Player.
    pub struct SessionBinding(Arc<dyn SessionDispatcher>);

    impl SessionBinding {
        /// Wraps the canonical session for one Host insertion.
        #[doc(hidden)]
        #[must_use]
        pub fn new(dispatcher: Arc<dyn SessionDispatcher>) -> Self {
            Self(dispatcher)
        }
    }

    struct SessionSlot {
        dispatcher: Mutex<Option<Arc<dyn SessionDispatcher>>>,
    }

    #[derive(Clone)]
    pub struct SessionHandle(Arc<SessionSlot>);

    impl SessionHandle {
        #[must_use]
        pub fn new(dispatcher: Arc<dyn SessionDispatcher>) -> Self {
            Self(Arc::new(SessionSlot {
                dispatcher: Mutex::new(Some(dispatcher)),
            }))
        }

        #[must_use]
        pub(crate) fn pending() -> Self {
            Self(Arc::new(SessionSlot {
                dispatcher: Mutex::default(),
            }))
        }

        pub(crate) fn bind(&self, binding: SessionBinding) -> Result<(), PlayError> {
            let mut dispatcher = self.0.dispatcher.lock();
            if dispatcher.is_some() {
                return Err(PlayError::SessionAlreadyBound);
            }
            *dispatcher = Some(binding.0);
            drop(dispatcher);
            Ok(())
        }

        pub fn allocate_slot(&self, player_id: PlayerId) -> Result<AllocatedSlot, PlayError> {
            match self.exec_ok(Cmd::AllocateSlot { player_id })? {
                Reply::SlotAllocated(allocated) => Ok(allocated),
                _ => Err(PlayError::Internal(
                    "unexpected reply for session allocate slot".into(),
                )),
            }
        }

        pub fn dispatcher(&self) -> Result<Arc<dyn SessionDispatcher>, PlayError> {
            self.0
                .dispatcher
                .lock()
                .clone()
                .ok_or(PlayError::SessionUnbound)
        }

        #[must_use]
        pub fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            // An instance may prepare resources before Host insertion. The
            // pending policy must therefore preserve the RT-safe production
            // path; explicit offline dispatchers override it after binding.
            self.dispatcher()
                .map_or(ConsumerWakeMode::RealtimeDeferred, |dispatcher| {
                    dispatcher.consumer_wake_mode()
                })
        }

        pub fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            self.dispatcher()?.exec(cmd)
        }

        pub fn exec_ok(&self, cmd: Cmd) -> Result<Reply, PlayError> {
            match self.exec(cmd)? {
                Reply::Err(err) => Err(PlayError::Session(err)),
                reply => Ok(reply),
            }
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
            grid_id: BeatGridId,
            bus: EventBus,
            eq_layout: Vec<EqBandConfig>,
            sample_pool: SamplePool,
            sample_rate: u32,
        ) -> Result<PlayerId, PlayError> {
            match self.exec_ok(Cmd::RegisterPlayer {
                grid_id,
                bus,
                eq_layout,
                sample_pool,
                sample_rate,
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

        #[cfg(any(test, feature = "probe"))]
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

pub use handle::{SessionBinding, SessionDispatcher, SessionHandle};
pub use wire::{AllocatedSlot, Cmd, PlayerId, PlayerLevel, Reply, SessionError, SessionSampleRate};

#[cfg(test)]
mod tests {
    use kithara_audio::ConsumerWakeMode;
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{Cmd, Reply, SessionBinding, SessionDispatcher, SessionHandle};
    use crate::PlayError;

    struct DefaultSession;

    impl SessionDispatcher for DefaultSession {
        fn exec(&self, _cmd: Cmd) -> Result<Reply, PlayError> {
            Ok(Reply::Ok)
        }

        fn consumer_wake_mode(&self) -> ConsumerWakeMode {
            ConsumerWakeMode::RealtimeDeferred
        }
    }

    #[kithara::test]
    fn session_handle_delegates_explicit_consumer_wake_mode() {
        let handle = SessionHandle::new(Arc::new(DefaultSession));

        assert_eq!(
            handle.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
    }

    #[kithara::test]
    fn pending_session_binds_once() {
        let handle = SessionHandle::pending();
        assert_eq!(
            handle.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
        assert!(matches!(
            handle.exec(Cmd::Tick),
            Err(PlayError::SessionUnbound)
        ));

        handle
            .bind(SessionBinding::new(Arc::new(DefaultSession)))
            .expect("bind canonical session");
        assert_eq!(
            handle.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
        assert!(matches!(handle.exec(Cmd::Tick), Ok(Reply::Ok)));
        assert!(matches!(
            handle.bind(SessionBinding::new(Arc::new(DefaultSession))),
            Err(PlayError::SessionAlreadyBound)
        ));
    }
}
