use kithara_sync::{TrackBeat, TrackGrid, TrackGridError};
use serde::{Deserialize, Serialize};

use super::{error::BeatGridError, raw::RawBeatGrid};

/// The wire schema this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// A checked wire document and its immutable mathematical geometry.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(into = "RawBeatGrid", try_from = "RawBeatGrid")]
pub struct BeatGridModel {
    raw: RawBeatGrid,
    geometry: TrackGrid,
}

impl BeatGridModel {
    /// The checked document, including publication identity and confidence.
    #[must_use]
    pub const fn as_raw(&self) -> &RawBeatGrid {
        &self.raw
    }

    /// Validated geometry shared by synchronization and map projection.
    #[must_use]
    pub const fn geometry(&self) -> &TrackGrid {
        &self.geometry
    }
}

impl From<BeatGridModel> for RawBeatGrid {
    fn from(model: BeatGridModel) -> Self {
        model.raw
    }
}

impl TryFrom<RawBeatGrid> for BeatGridModel {
    type Error = BeatGridError;

    fn try_from(raw: RawBeatGrid) -> Result<Self, Self::Error> {
        if raw.schema_version != SCHEMA_VERSION {
            return Err(BeatGridError::Schema {
                expected: SCHEMA_VERSION,
                found: raw.schema_version,
            });
        }
        if raw.model_id.is_empty() {
            return Err(BeatGridError::ModelId);
        }
        let geometry = TrackGrid::new(
            raw.beats
                .iter()
                .map(|beat| TrackBeat {
                    at: beat.at,
                    ordinal: beat.ordinal,
                })
                .collect(),
            raw.downbeats
                .iter()
                .map(|beat| TrackBeat {
                    at: beat.at,
                    ordinal: beat.beat_ordinal,
                })
                .collect(),
            raw.meter,
            raw.bpm,
            raw.duration,
        )
        .map_err(geometry_error)?;
        for confidence in raw
            .beats
            .iter()
            .map(|beat| beat.confidence)
            .chain(raw.downbeats.iter().map(|beat| beat.confidence))
            .flatten()
        {
            if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                return Err(BeatGridError::Confidence { confidence });
            }
        }
        Ok(Self { raw, geometry })
    }
}

/// Keeps wire error variants while sync owns geometric validation.
fn geometry_error(error: TrackGridError) -> BeatGridError {
    match error {
        TrackGridError::Bpm { bpm } => BeatGridError::Bpm { bpm },
        TrackGridError::Time { seconds } => BeatGridError::Time { seconds },
        TrackGridError::Order { ordinal, seconds } => BeatGridError::Order { ordinal, seconds },
        TrackGridError::PastDuration {
            ordinal,
            seconds,
            duration,
        } => BeatGridError::PastDuration {
            ordinal,
            seconds,
            duration,
        },
        TrackGridError::Anchor { ordinal } => BeatGridError::Anchor { ordinal },
        TrackGridError::Meter {
            beats_per_bar,
            ordinal,
        } => BeatGridError::Meter {
            beats_per_bar,
            ordinal,
        },
    }
}
