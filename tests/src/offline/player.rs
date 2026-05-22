use std::sync::{Arc, atomic::Ordering};

use firewheel::{FirewheelConfig, FirewheelCtx, channel_config::ChannelCount};
use kithara_audio::PcmReader;
use kithara_platform::Mutex;
use kithara_play::{
    PlayerNode, Resource,
    impls::{
        player_processor::PlayerCmd, player_resource::PlayerResource,
        player_track::TrackTransition, shared_player_state::SharedPlayerState,
    },
};
use ringbuf::{
    HeapRb,
    traits::{Producer, Split},
};

use super::backend::{OfflineBackend, OfflineConfig};

pub struct OfflinePlayer {
    shared_state: Arc<SharedPlayerState>,
    ctx: FirewheelCtx<OfflineBackend>,
    cmd_tx: ringbuf::HeapProd<PlayerCmd>,
}

impl OfflinePlayer {
    const CMD_RINGBUF_CAPACITY: usize = 64;
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
            .expect("BUG: start offline stream");

        let shared_state = Arc::new(SharedPlayerState::new());
        let (cmd_tx, cmd_rx) = HeapRb::new(Self::CMD_RINGBUF_CAPACITY).split();

        let player_node = PlayerNode::with_channel(
            cmd_rx,
            Arc::clone(&shared_state),
            kithara_bufpool::PcmPool::default().clone(),
        );
        let node_id = ctx.add_node(player_node, None);
        let graph_out = ctx.graph_out_node_id();
        ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
            .expect("BUG: connect player to output");
        ctx.update().expect("BUG: initial graph update");

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
    pub fn load_and_fadein(&mut self, resource: Resource, src: &str) {
        let src: Arc<str> = Arc::from(src);
        let pr = PlayerResource::new(
            resource,
            Arc::clone(&src),
            &kithara_bufpool::PcmPool::default(),
        );
        self.cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: Arc::new(Mutex::new(pr)),
                src: Arc::clone(&src),
                item_id: None,
            })
            .expect("BUG: send LoadTrack");
        self.cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(src)))
            .expect("BUG: send FadeIn");
        self.cmd_tx
            .try_push(PlayerCmd::SetPaused(false))
            .expect("BUG: send SetPaused");
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
        self.ctx.update().expect("BUG: graph update");
        self.ctx
            .active_backend_mut()
            .expect("BUG: backend active")
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
            .expect("BUG: send Seek");
    }

    /// Drain pending processor notifications.
    ///
    /// Tests use this to discriminate between a track that ended/failed
    /// cleanly (notification arrives within the observation window) and
    /// a pipeline stuck in a recreate-loop (no terminal notification —
    /// position stays pinned at the seek target).
    pub fn take_notification_kinds(&self) -> Vec<NotificationKind> {
        use ringbuf::traits::Consumer;

        let mut rx = self.shared_state.notification_rx.lock_sync();
        let mut out = Vec::new();
        while let Some(n) = rx.try_pop() {
            use kithara_play::impls::player_notification::PlayerNotification as N;
            out.push(match n {
                N::Loaded { .. } => NotificationKind::Loaded,
                N::Unloaded { .. } => NotificationKind::Unloaded,
                N::HandoverRequested => NotificationKind::HandoverRequested,
                N::PlaybackStarted => NotificationKind::PlaybackStarted,
                N::PlaybackStopped { .. } => NotificationKind::PlaybackStopped,
                N::Requested => NotificationKind::Requested,
                N::Changed { .. } => NotificationKind::Changed,
                N::FadingIn => NotificationKind::FadingIn,
                N::FadingOut => NotificationKind::FadingOut,
            });
        }
        out
    }
}

/// Tag for `PlayerNotification` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Loaded,
    Unloaded,
    HandoverRequested,
    PlaybackStarted,
    PlaybackStopped,
    Requested,
    Changed,
    FadingIn,
    FadingOut,
}

/// Thin wrapper around [`Resource::from_reader`] for tests.
pub fn resource_from_reader<R>(reader: R) -> Resource
where
    R: PcmReader + 'static,
{
    Resource::from_reader(reader, None)
}

/// Thin wrapper around [`Resource::from_reader`] with an explicit `src`
/// tag, used by harness tests that match `ItemDidPlayToEnd { src, .. }`
/// on the bus.
pub fn resource_from_reader_with_src<R, S>(reader: R, src: S) -> Resource
where
    R: PcmReader + 'static,
    S: Into<Arc<str>>,
{
    Resource::from_reader(reader, Some(src.into()))
}
