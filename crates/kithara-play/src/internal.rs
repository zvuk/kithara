#![forbid(unsafe_code)]

pub mod engine {
    #[rustfmt::skip]
    pub use crate::traits::dj::crossfade::CrossfadeConfig;
    #[rustfmt::skip]
    pub use crate::traits::engine::Engine;
    pub use crate::{EngineConfig, EngineEvent, EngineImpl, PlayError, SessionDuckingMode, SlotId};

    #[must_use]
    pub fn slot_id(value: u64) -> SlotId {
        SlotId::new(value)
    }
}

/// Initialise the global session client with an offline (test-only)
/// audio backend.
///
/// Must be called **before** any `PlayerImpl::new()` / `Queue::new()`.
/// Idempotent: a second call is a no-op if the offline client is
/// already set. If production code raced ahead and initialised the
/// cpal client first, this panics — the caller should have called
/// this function earlier in the test setup.
///
/// Available with the `test-utils` feature on both native and wasm.
/// On wasm, offline mode is a thread-local opt-in; on native it swaps
/// the global session client before first use.
///
/// # Panics
///
/// Panics if the global session client was already initialised with a
/// non-offline backend (cpal on native, `WebAudio` on wasm). Call this
/// before any `PlayerImpl::new()` or `Queue::new()` in the test binary.
#[cfg(any(test, feature = "test-utils"))]
pub fn init_offline_backend() {
    crate::impls::session_engine::try_init_offline_session()
        .expect("failed to initialise offline session backend");
}

#[cfg(any(test, feature = "test-utils"))]
pub mod offline {
    use std::sync::{Arc, atomic::Ordering};

    use firewheel::{FirewheelConfig, FirewheelCtx, channel_config::ChannelCount};
    use kithara_platform::Mutex;
    use ringbuf::{
        HeapRb,
        traits::{Consumer, Producer, Split},
    };

    use crate::impls::{
        offline_backend::{OfflineBackend, OfflineConfig},
        player_node::PlayerNode,
        player_processor::PlayerCmd,
        player_resource::PlayerResource,
        player_track::TrackTransition,
        shared_player_state::SharedPlayerState,
    };

    pub struct OfflinePlayer {
        shared_state: Arc<SharedPlayerState>,
        ctx: FirewheelCtx<OfflineBackend>,
        cmd_tx: ringbuf::HeapProd<PlayerCmd>,
    }

    impl OfflinePlayer {
        /// Capacity of the player command ring buffer.
        const CMD_RINGBUF_CAPACITY: usize = 64;

        /// Offline audio block size in frames.
        const OFFLINE_BLOCK_FRAMES: u32 = 512;

        /// Create an offline player with stereo output at the given sample rate.
        ///
        /// # Panics
        ///
        /// Panics if the offline audio stream or graph cannot be initialised.
        #[must_use]
        pub fn new(sample_rate: u32) -> Self {
            let fw_config = FirewheelConfig {
                num_graph_outputs: ChannelCount::STEREO,
                ..FirewheelConfig::default()
            };
            let mut ctx = FirewheelCtx::<OfflineBackend>::new(fw_config);

            let stream_config = OfflineConfig {
                sample_rate,
                block_frames: Self::OFFLINE_BLOCK_FRAMES,
            };
            ctx.start_stream(stream_config)
                .expect("start offline stream");

            let shared_state = Arc::new(SharedPlayerState::new());
            let (cmd_tx, cmd_rx) = HeapRb::new(Self::CMD_RINGBUF_CAPACITY).split();

            let player_node = PlayerNode::with_channel(
                cmd_rx,
                Arc::clone(&shared_state),
                // OfflinePlayer test-utils fixture
                // ast-grep-ignore: perf.no-global-pool-accessor
                kithara_bufpool::PcmPool::default().clone(),
            );
            let node_id = ctx.add_node(player_node, None);
            let graph_out = ctx.graph_out_node_id();
            ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
                .expect("connect player to output");
            ctx.update().expect("initial graph update");

            Self {
                shared_state,
                ctx,
                cmd_tx,
            }
        }

        /// Load a resource as a track and trigger `FadeIn` crossfade.
        ///
        /// # Panics
        ///
        /// Panics if the command channel is full.
        pub fn load_and_fadein(&mut self, resource: crate::Resource, src: &str) {
            let src: Arc<str> = Arc::from(src);
            let pr = PlayerResource::new(
                resource,
                Arc::clone(&src),
                // OfflinePlayer test-utils fixture
                // ast-grep-ignore: perf.no-global-pool-accessor
                &kithara_bufpool::PcmPool::default(),
            );
            self.cmd_tx
                .try_push(PlayerCmd::LoadTrack {
                    resource: Arc::new(Mutex::new(pr)),
                    src: Arc::clone(&src),
                })
                .expect("send LoadTrack");
            self.cmd_tx
                .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(src)))
                .expect("send FadeIn");
            self.cmd_tx
                .try_push(PlayerCmd::SetPaused(false))
                .expect("send SetPaused");
        }

        /// Current playback position in seconds.
        pub fn position(&self) -> f64 {
            self.shared_state.position.load(Ordering::Relaxed)
        }

        /// Number of times `process()` was called.
        pub fn process_count(&self) -> u64 {
            self.shared_state.process_count.load(Ordering::Relaxed)
        }

        /// Render `frames` of audio. Returns interleaved stereo output.
        ///
        /// # Panics
        ///
        /// Panics if the graph update or backend access fails.
        pub fn render(&mut self, frames: usize) -> Vec<f32> {
            self.ctx.update().expect("graph update");
            self.ctx
                .active_backend_mut()
                .expect("backend active")
                .render(frames)
        }

        /// Send a seek command to the processor.
        ///
        /// # Panics
        ///
        /// Panics if the command channel is full.
        pub fn seek(&mut self, seconds: f64, seek_epoch: u64) {
            self.shared_state
                .seek_epoch
                .store(seek_epoch, Ordering::SeqCst);
            self.cmd_tx
                .try_push(PlayerCmd::Seek {
                    seconds,
                    seek_epoch,
                })
                .expect("send Seek");
        }

        /// Drain pending processor notifications.
        ///
        /// Tests use this to discriminate between a track that ended/failed
        /// cleanly (notification arrives within the observation window) and
        /// a pipeline stuck in a recreate-loop (no terminal notification —
        /// position stays pinned at the seek target).
        ///
        /// Returns notification kind tags rather than the full enum so the
        /// internal `PlayerNotification` type stays `pub(crate)`.
        pub fn take_notification_kinds(&self) -> Vec<NotificationKind> {
            let mut rx = self.shared_state.notification_rx.lock_sync();
            let mut out = Vec::new();
            while let Some(n) = rx.try_pop() {
                use crate::impls::player_notification::PlayerNotification as N;
                out.push(match n {
                    N::TrackError(_, _) => NotificationKind::TrackError,
                    N::TrackLoaded(_) => NotificationKind::TrackLoaded,
                    N::TrackUnloaded(_) => NotificationKind::TrackUnloaded,
                    N::TrackAboutToEnd(_) => NotificationKind::TrackAboutToEnd,
                    N::TrackPlaybackStarted(_) => NotificationKind::TrackPlaybackStarted,
                    N::TrackPlaybackStopped(_) => NotificationKind::TrackPlaybackStopped,
                    N::TrackPlaybackPaused(_) => NotificationKind::TrackPlaybackPaused,
                    N::TrackRequested(_) => NotificationKind::TrackRequested,
                    N::TrackChanged { .. } => NotificationKind::TrackChanged,
                    N::TrackFadingIn(_) => NotificationKind::TrackFadingIn,
                    N::TrackFadingOut(_) => NotificationKind::TrackFadingOut,
                });
            }
            out
        }
    }

    /// Tag for [`PlayerNotification`] variants.
    ///
    /// Mirrors the variants of the internal `PlayerNotification` enum so
    /// tests can match on terminal-state classes (`TrackError`,
    /// `TrackPlaybackStopped`) without importing the `pub(crate)` type.
    ///
    /// [`PlayerNotification`]: crate::impls::player_notification::PlayerNotification
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NotificationKind {
        TrackError,
        TrackLoaded,
        TrackUnloaded,
        TrackAboutToEnd,
        TrackPlaybackStarted,
        TrackPlaybackStopped,
        TrackPlaybackPaused,
        TrackRequested,
        TrackChanged,
        TrackFadingIn,
        TrackFadingOut,
    }

    /// Create a [`Resource`](crate::Resource) from any [`PcmReader`].
    ///
    /// Test-only wrapper around the `pub(crate)` constructor.
    ///
    /// [`PcmReader`]: kithara_audio::PcmReader
    #[expect(clippy::impl_trait_in_params, reason = "test utility, ergonomic API")]
    pub fn resource_from_reader(
        reader: impl kithara_audio::PcmReader + 'static,
    ) -> crate::Resource {
        crate::Resource::from_reader(reader)
    }
}

pub use crate::{
    ActionAtItemEnd, DjEvent, EngineConfig, EngineEvent, EngineImpl, ItemStatus, MediaTime,
    ObserverId, PlayError, PlayerConfig, PlayerEvent, PlayerImpl, PlayerStatus, Resource,
    ResourceConfig, ResourceSrc, SessionDuckingMode, SessionEvent, SlotId, SourceType,
    TimeControlStatus, TimeRange, WaitingReason,
};
