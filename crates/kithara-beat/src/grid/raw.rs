use kithara_sync::Meter;
use serde::{Deserialize, Serialize};

/// Whether the producer may still revise the grid it published.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BeatGridState {
    Provisional,
    Final,
}

/// One beat of the grid: where it falls, which beat it is, and how sure
/// whoever placed it was.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct GridBeat {
    /// Absent where nothing observed the beat and it was placed by fitting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Media seconds from the start of the track.
    pub at: f64,
    /// The beat's own number, stable across revisions and across the gaps a
    /// sparse grid leaves. Never a position in [`RawBeatGrid::beats`].
    pub ordinal: i64,
}

/// One bar line, named by the beat it falls on.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct GridDownbeat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Media seconds, equal to the time the named beat carries.
    pub at: f64,
    pub beat_ordinal: i64,
}

/// A beat grid as it arrives over the wire: a document nobody has checked yet.
///
/// Reading a [`BeatGridModel`](super::BeatGridModel) goes through this, so no
/// deserialization reaches a validated grid without the checks.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RawBeatGrid {
    pub state: BeatGridState,
    /// Media seconds the track is known to run for; absent where the length is
    /// not known, which is a different answer from zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<Meter>,
    pub model_id: String,
    #[serde(default)]
    pub beats: Vec<GridBeat>,
    #[serde(default)]
    pub downbeats: Vec<GridDownbeat>,
    pub bpm: f64,
    pub schema_version: u32,
    pub revision: u64,
}
