use core::sync::atomic::AtomicU64;

use kithara_audio::SeekBegin;
use kithara_platform::{sync::Arc, time::Duration};
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};
use smallvec::SmallVec;

use super::PlaybackShared;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, SharedEq},
    rt::{PlayerNodeProcessor, track::PlayerTrack},
};

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
pub struct NodeInputs {
    pub(crate) playback: Arc<PlaybackShared>,
    pub(crate) cmd_rx: HeapCons<PlayerCmd>,
    pub(crate) notif_tx: HeapProd<PlayerNotification>,
    pub(crate) trash_tx: HeapProd<PlayerTrack>,
}

/// Producer for interleaved stereo mix samples and their drop count.
#[non_exhaustive]
pub struct MixTapWriter {
    pub(crate) pcm: HeapProd<f32>,
    pub(crate) drops: Arc<AtomicU64>,
}

impl MixTapWriter {
    #[must_use]
    pub fn new(pcm: HeapProd<f32>, drops: Arc<AtomicU64>) -> Self {
        Self { pcm, drops }
    }
}

/// Control-owned channel halves and shared controls for one allocated slot.
#[non_exhaustive]
pub struct SlotControl {
    pub playback: Arc<PlaybackShared>,
    pub notif_rx: HeapCons<PlayerNotification>,
    pub trash_rx: HeapCons<PlayerTrack>,
    pub cmd_tx: HeapProd<PlayerCmd>,
    pub eq: SharedEq,
    seek: SeekBindings,
}

#[derive(Default)]
struct SeekBindings(SmallVec<[SeekBinding; SLOT_TRACKS]>);

type SeekBinding = (Arc<str>, Arc<dyn SeekBegin>);

const SLOT_TRACKS: usize = PlayerNodeProcessor::MAX_TRACKS;

impl SlotControl {
    /// Record the control half of a track's seek path.
    pub fn bind_seek(&mut self, src: Arc<str>, handle: Arc<dyn SeekBegin>) {
        self.unbind_seek(&src);
        self.seek.0.push((src, handle));
    }

    /// Begin a seek on every track this slot holds, off the audio thread.
    pub fn begin_seek(&self, position: Duration) {
        for (_, handle) in &self.seek.0 {
            handle.begin(position);
        }
    }

    /// Forget a track's seek path once the processor reports it unloaded.
    pub fn unbind_seek(&mut self, src: &str) {
        self.seek.0.retain(|(bound, _)| &**bound != src);
    }
}

#[must_use]
pub fn slot_channels(eq: SharedEq) -> (NodeInputs, SlotControl) {
    const COMMAND_CAPACITY: usize = 32;
    const NOTIFICATION_CAPACITY: usize = 32;
    const TRASH_CAPACITY: usize = 64;

    let (cmd_tx, cmd_rx) = HeapRb::<PlayerCmd>::new(COMMAND_CAPACITY).split();
    let (notif_tx, notif_rx) = HeapRb::<PlayerNotification>::new(NOTIFICATION_CAPACITY).split();
    let (trash_tx, trash_rx) = HeapRb::<PlayerTrack>::new(TRASH_CAPACITY).split();
    let playback = Arc::new(PlaybackShared::default());

    let inputs = NodeInputs {
        cmd_rx,
        notif_tx,
        trash_tx,
        playback: Arc::clone(&playback),
    };
    let control = SlotControl {
        playback,
        notif_rx,
        trash_rx,
        cmd_tx,
        eq,
        seek: SeekBindings::default(),
    };
    (inputs, control)
}
