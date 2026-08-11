use kithara_audio::SessionBeat;

use crate::api::TransportRevision;

/// When a preloading track becomes audible.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum TrackStart {
    /// The leading track runs out and this one takes over at that offset. The
    /// queue's own behaviour, and what a track does unless it is told
    /// otherwise.
    #[default]
    Handover,
    /// A stamped session beat. The revision is part of the plan, not a hint:
    /// a start computed against a transport that has since been re-committed
    /// would land on a different frame, so it is dropped rather than applied.
    Session {
        target: SessionBeat,
        revision: TransportRevision,
    },
}
