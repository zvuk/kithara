use kithara_beat::BeatGridModel;
use kithara_command::Seq;
use kithara_events::TrackId;

/// One analysis answer, scoped to the item and exact load that requested it.
#[derive(Clone, Debug)]
pub struct GridAnswer {
    pub item: TrackId,
    pub load: Seq,
    pub model: Result<BeatGridModel, GridRefusal>,
}

/// Analysis cannot supply a usable grid for this load.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("beat-grid analysis refused: {reason}")]
pub struct GridRefusal {
    pub reason: String,
}
