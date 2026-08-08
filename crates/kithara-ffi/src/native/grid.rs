use std::num::NonZeroU32;

use kithara::audio::{BeatGrid, analysis::TrackAnalysis};

use crate::types::FfiError;

/// One track's analysed beat grid, as the integrating application holds it.
///
/// Kithara does not analyse tracks on behalf of an FFI host: the host owns its
/// catalogue and usually already has a grid there. This record is the host's
/// grid in Kithara's own coordinates — marker positions in decoded source
/// frames, on the sample-rate axis those frames are counted at — so nothing is
/// re-derived on the way in and a drifting grid stays drifting.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FfiTrackGrid {
    /// Nominal tempo of the track in beats per minute.
    pub bpm: f64,
    /// Beat positions in decoded source frames, strictly increasing. At least
    /// two are needed: one marker defines no interval to interpolate over.
    pub beat_frames: Vec<u64>,
    /// The subset of `beat_frames` that are downbeats. May be empty.
    pub downbeat_frames: Vec<u64>,
    /// Total decoded source frames of the track: the domain of the grid.
    pub source_frames: u64,
    /// Sample rate the frame positions above are counted at.
    pub source_sample_rate: u32,
}

impl TryFrom<FfiTrackGrid> for TrackAnalysis {
    type Error = FfiError;

    fn try_from(grid: FfiTrackGrid) -> Result<Self, Self::Error> {
        let source_sample_rate =
            NonZeroU32::new(grid.source_sample_rate).ok_or_else(|| FfiError::InvalidArgument {
                reason: "beat grid sample rate must be non-zero".to_owned(),
            })?;
        Ok(Self::with_source_rate(
            Some(BeatGrid::new(
                grid.bpm,
                grid.beat_frames,
                grid.downbeat_frames,
                Vec::new(),
            )),
            None,
            grid.source_frames,
            source_sample_rate,
        ))
    }
}
