use kithara_platform::sync::Arc;
use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};

use super::PlaybackShared;
use crate::{
    bridge::{PlayerCmd, PlayerNotification, SharedEq},
    rt::{
        TempoParticipantControl, TempoParticipantEndpoint, tempo_participant_channel,
        track::PlayerTrack,
    },
};

/// RT-owned channel halves and playback atomics for one player node.
#[non_exhaustive]
pub struct NodeInputs {
    pub(crate) cmd_rx: HeapCons<PlayerCmd>,
    pub(crate) notif_tx: HeapProd<PlayerNotification>,
    pub(crate) trash_tx: HeapProd<PlayerTrack>,
    pub(crate) playback: Arc<PlaybackShared>,
    pub(crate) tempo: Option<TempoParticipantEndpoint>,
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
        tempo: None,
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

#[must_use]
pub(crate) fn session_slot_channels(
    eq: SharedEq,
) -> (NodeInputs, SlotControl, TempoParticipantControl) {
    let (mut inputs, control) = slot_channels(eq);
    let (endpoint, tempo) = tempo_participant_channel();
    inputs.tempo = Some(endpoint);
    (inputs, control, tempo)
}
