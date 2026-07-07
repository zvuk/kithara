use std::sync::Arc;

use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};

use super::PlaybackShared;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, SharedEq},
    rt::track::PlayerTrack,
};

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
pub struct NodeInputs {
    pub cmd_rx: HeapCons<PlayerCmd>,
    pub notif_tx: HeapProd<PlayerNotification>,
    pub trash_tx: HeapProd<PlayerTrack>,
    pub playback: Arc<PlaybackShared>,
}

/// Control-owned channel halves and shared controls for one allocated slot.
#[non_exhaustive]
pub struct SlotControl {
    pub cmd_tx: HeapProd<PlayerCmd>,
    pub notif_rx: HeapCons<PlayerNotification>,
    pub trash_rx: HeapCons<PlayerTrack>,
    pub playback: Arc<PlaybackShared>,
    pub eq: SharedEq,
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
        cmd_tx,
        notif_rx,
        trash_rx,
        playback,
        eq,
    };
    (inputs, control)
}
