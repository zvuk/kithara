use kithara_beat::BeatGridModel;
use kithara_events::TrackId;

/// One analysis answer for the track of one item.
#[derive(Clone, Debug, PartialEq)]
pub struct GridAnswer {
    pub item: TrackId,
    pub model: Result<BeatGridModel, GridRefusal>,
}

/// Analysis cannot supply a usable grid for this item.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("beat-grid analysis refused: {reason}")]
pub struct GridRefusal {
    pub reason: String,
}
