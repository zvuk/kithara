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

#[cfg(any(test, feature = "test-utils"))]
pub mod offline {
    use std::sync::{Arc, atomic::Ordering};

    use firewheel::{FirewheelConfig, FirewheelCtx, channel_config::ChannelCount};
    use kithara_platform::Mutex;
    use ringbuf::{
        HeapRb,
        traits::{Producer, Split},
    };

    use crate::impls::{
        offline_backend::{OfflineBackend, OfflineConfig},
        player_node::PlayerNode,
        player_processor::PlayerCmd,
        player_resource::PlayerResource,
        player_track::TrackTransition,
        shared_player_state::SharedPlayerState,
    };

    /// Offline audio block size in frames.
    const OFFLINE_BLOCK_FRAMES: u32 = 512;

    /// Capacity of the player command ring buffer.
    const CMD_RINGBUF_CAPACITY: usize = 64;

    /// A self-contained offline player for testing crossfade and mixing.
    ///
    /// Wraps [`FirewheelCtx<OfflineBackend>`] with a player node. Call
    /// [`render`](Self::render) to step the audio graph and inspect
    /// the rendered output for gaps.
    pub struct OfflinePlayer {
        ctx: FirewheelCtx<OfflineBackend>,
        cmd_tx: ringbuf::HeapProd<PlayerCmd>,
        shared_state: Arc<SharedPlayerState>,
    }

    impl OfflinePlayer {
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
                block_frames: OFFLINE_BLOCK_FRAMES,
            };
            ctx.start_stream(stream_config)
                .expect("start offline stream");

            let shared_state = Arc::new(SharedPlayerState::new());
            let (cmd_tx, cmd_rx) = HeapRb::new(CMD_RINGBUF_CAPACITY).split();

            let player_node = PlayerNode::with_channel(
                cmd_rx,
                Arc::clone(&shared_state),
                kithara_bufpool::pcm_pool().clone(),
            );
            let node_id = ctx.add_node(player_node, None);
            let graph_out = ctx.graph_out_node_id();
            ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
                .expect("connect player to output");
            ctx.update().expect("initial graph update");

            Self {
                ctx,
                cmd_tx,
                shared_state,
            }
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

        /// Load a resource as a track and trigger `FadeIn` crossfade.
        ///
        /// # Panics
        ///
        /// Panics if the command channel is full.
        pub fn load_and_fadein(&mut self, resource: crate::Resource, src: &str) {
            let src: Arc<str> = Arc::from(src);
            let pr = PlayerResource::new(resource, Arc::clone(&src), kithara_bufpool::pcm_pool());
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
