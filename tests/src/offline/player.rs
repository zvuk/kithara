use std::sync::{Arc, atomic::Ordering};

use firewheel::{FirewheelConfig, FirewheelCtx, channel_config::ChannelCount};
use kithara::{
    audio::PcmReader,
    play::{
        PlayerNode, Resource, SharedEq, TrackTransition,
        bridge::{PlaybackShared, PlayerCmd, SlotControl, slot_channels},
        rt::track::PlayerResource,
    },
};
use ringbuf::traits::{Consumer, Producer};

use super::backend::{OfflineBackend, OfflineConfig};

pub struct OfflinePlayer {
    playback: Arc<PlaybackShared>,
    ctx: FirewheelCtx<OfflineBackend>,
    control: SlotControl,
}

impl OfflinePlayer {
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

        let (inputs, control) = slot_channels(SharedEq::new(0));
        let playback = Arc::clone(&control.playback);

        let player_node = PlayerNode::new(inputs, kithara::bufpool::PcmPool::default().clone());
        let node_id = ctx.add_node(player_node, None);
        let graph_out = ctx.graph_out_node_id();
        ctx.connect(node_id, graph_out, &[(0, 0), (1, 1)], false)
            .expect("BUG: connect player to output");
        ctx.update().expect("BUG: initial graph update");

        Self {
            playback,
            ctx,
            control,
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
            &kithara::bufpool::PcmPool::default(),
        );
        self.control
            .cmd_tx
            .try_push(PlayerCmd::LoadTrack {
                resource: Box::new(pr),
                item_id: None,
            })
            .expect("BUG: send LoadTrack");
        self.control
            .cmd_tx
            .try_push(PlayerCmd::Transition(TrackTransition::FadeIn(src)))
            .expect("BUG: send FadeIn");
        self.control
            .cmd_tx
            .try_push(PlayerCmd::SetPaused(false))
            .expect("BUG: send SetPaused");
    }

    /// Current playback position in seconds.
    pub fn position(&self) -> f64 {
        self.playback.position.load(Ordering::Relaxed)
    }

    /// Number of times `process()` was called.
    pub fn process_count(&self) -> u64 {
        self.playback.process_count.load(Ordering::Relaxed)
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
        self.playback.seek_epoch.store(seek_epoch, Ordering::SeqCst);
        self.control
            .cmd_tx
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
    pub fn take_notification_kinds(&mut self) -> Vec<NotificationKind> {
        let mut out = Vec::new();
        while let Some(n) = self.control.notif_rx.try_pop() {
            use kithara::play::PlayerNotification as N;
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
        while self.control.trash_rx.try_pop().is_some() {}
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
