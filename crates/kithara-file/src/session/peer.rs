//! `Peer` implementation for the file protocol.
//!
//! Reports priority based on the audio FSM's PLAYING flag exposed via the
//! shared `Timeline` — fetches for an actively-consumed track go to the
//! High-priority slot, idle ones go to Low.

use kithara_abr::Abr;
use kithara_stream::{
    Timeline,
    dl::{Peer, Priority},
};

pub(crate) struct FilePeer {
    /// Same Arc-clone as the one held by `FileCoord` — reads from the
    /// audio FSM's PLAYING flag route this track's fetches to the
    /// High-priority slot while the listener is actively consuming it.
    timeline: Timeline,
}

impl FilePeer {
    pub(crate) fn new(timeline: Timeline) -> Self {
        Self { timeline }
    }
}

impl Abr for FilePeer {
    // File streams have no variants and no buffer signal — all defaults.
}

impl Peer for FilePeer {
    /// Priority reflects the audio FSM's decode-activity flag on the
    /// shared `Timeline`. Cheap, lock-free — called by Registry on
    /// every `poll_peers` pass.
    fn priority(&self) -> Priority {
        if self.timeline.is_playing() {
            Priority::High
        } else {
            Priority::Low
        }
    }
}
