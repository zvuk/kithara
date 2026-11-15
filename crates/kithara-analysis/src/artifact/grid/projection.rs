use std::num::NonZeroU16;

use kithara_beat::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, RawBeatGrid,
    SCHEMA_VERSION,
};
use kithara_signal::{AudioSpec, SignalError};
use kithara_sync::Meter;
use num_traits::cast::ToPrimitive;
use thiserror::Error;

use super::fit;
use crate::artifact::{
    meter::voted_bar,
    snapshot::{BeatSnapshot, BeatState},
    track::TrackAnalysis,
};

/// How far a marker may sit from a whole beat and still name that beat without
/// a second reading. Past a quarter beat the nearest ordinal is a guess, and a
/// guess is not an observation, so that marker is omitted from the grid.
pub const ORDINAL_TOLERANCE_BEATS: f64 = 0.25;

/// Why a pass states no grid a player could follow.
///
/// The pass keeps its own artifact either way: a grid it cannot state is not a
/// waveform it cannot publish.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum BeatGridUnavailable {
    #[error("the pass published no beat artifact")]
    NoBeats,
    #[error("the source states no original media clock")]
    MediaOrigin,
    #[error(transparent)]
    SourceClock(#[from] SignalError),
    #[error("source extent {end} precedes media zero {origin}")]
    MediaExtent { origin: u64, end: u64 },
    #[error(transparent)]
    Rejected(#[from] BeatGridError),
    #[error("the artifact carries {bpm} where a tempo would be")]
    Tempo { bpm: f64 },
}

/// Restates one published pass as the server-side grid contract.
///
/// This is the only place source frames become media seconds: the artifact
/// keeps frames, and the model has no sample rate to read them against.
impl TryFrom<&TrackAnalysis> for BeatGridModel {
    type Error = BeatGridUnavailable;

    fn try_from(analysis: &TrackAnalysis) -> Result<Self, Self::Error> {
        let snapshot = analysis.beat().ok_or(BeatGridUnavailable::NoBeats)?;
        let bpm = snapshot.artifact().bpm();
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(BeatGridUnavailable::Tempo { bpm });
        }
        let spec = AudioSpec::new(1, analysis.source_sample_rate());
        let origin = spec.frame_at(
            analysis
                .media_origin()
                .ok_or(BeatGridUnavailable::MediaOrigin)?,
        )?;
        let rate = f64::from(spec.sample_rate.get());
        let duration = analysis
            .extent()
            .map(|extent| {
                extent
                    .checked_sub(origin)
                    .map(|frames| seconds(frames, rate))
                    .ok_or(BeatGridUnavailable::MediaExtent {
                        origin,
                        end: extent,
                    })
            })
            .transpose()?;
        let (mut placed, bpm) = place(snapshot, rate, bpm, duration, origin);
        placed.retain(|(_, beat)| !duration.is_some_and(|duration| beat.at > duration));
        let (downbeats, meter) = bars(snapshot, &placed);
        Ok(Self::try_from(RawBeatGrid {
            duration,
            bpm,
            downbeats,
            meter,
            schema_version: SCHEMA_VERSION,
            model_id: analysis.token().as_str().to_owned(),
            revision: analysis.revision(),
            state: match snapshot.state() {
                BeatState::Final => BeatGridState::Final,
                BeatState::Provisional => BeatGridState::Provisional,
            },
            beats: placed.iter().map(|(_, beat)| *beat).collect(),
        })?)
    }
}

/// Keeps source-frame identity while fitting markers on the media clock.
fn place(
    snapshot: &BeatSnapshot,
    rate: f64,
    bpm: f64,
    duration: Option<f64>,
    origin: u64,
) -> (Vec<(u64, GridBeat)>, f64) {
    let artifact = snapshot.artifact();
    let markers: Vec<(u64, GridBeat)> = artifact
        .beats()
        .iter()
        .zip(artifact.beat_confidence())
        .filter_map(|(frame, confidence)| {
            let frame_after_origin = frame.checked_sub(origin)?;
            let beat = GridBeat {
                at: seconds(frame_after_origin, rate),
                ordinal: 0,
                confidence: *confidence,
            };
            Some((*frame, beat))
        })
        .filter(|(_, beat)| !duration.is_some_and(|duration| beat.at > duration))
        .collect();
    fit::place(&markers, bpm)
}

/// The bar lines the detected ones agree on, each named by the beat it falls
/// on, and the meter they keep.
///
/// Only a detected bar line on a placed beat votes; the bar and phase the
/// votes agree on then name every placed beat of that phase a bar line, so a
/// bar the detector skipped is stated without a confidence of its own and a
/// bar line on the wrong beat is left out. Votes that agree on nothing state
/// no bars at all.
fn bars(snapshot: &BeatSnapshot, placed: &[(u64, GridBeat)]) -> (Vec<GridDownbeat>, Option<Meter>) {
    let artifact = snapshot.artifact();
    let heard: Vec<(usize, Option<f32>)> = artifact
        .downbeats()
        .iter()
        .zip(artifact.downbeat_confidence().iter())
        .filter_map(|(frame, confidence)| {
            let index = placed
                .binary_search_by_key(frame, |(placed, _)| *placed)
                .ok()?;
            Some((index, *confidence))
        })
        .collect();
    let votes = heard
        .iter()
        .filter(|(_, confidence)| confidence.is_some())
        .map(|(index, _)| placed[*index].1.ordinal);
    let Some((bar, phase)) = voted_bar(votes) else {
        return (Vec::new(), None);
    };
    let downbeats: Vec<GridDownbeat> = placed
        .iter()
        .enumerate()
        .filter(|(_, (_, beat))| beat.ordinal.rem_euclid(bar) == phase)
        .map(|(index, (_, beat))| GridDownbeat {
            at: beat.at,
            beat_ordinal: beat.ordinal,
            confidence: heard
                .binary_search_by_key(&index, |(heard, _)| *heard)
                .ok()
                .and_then(|found| heard[found].1),
        })
        .collect();
    let meter = bar
        .to_u16()
        .and_then(NonZeroU16::new)
        .zip(downbeats.first())
        .map(|(beats_per_bar, first)| Meter {
            beats_per_bar,
            origin_beat_ordinal: first.beat_ordinal,
        });
    (downbeats, meter)
}

/// A frame the timeline cannot represent becomes a position the contract
/// refuses, rather than one it silently rounds.
fn seconds(frame: u64, rate: f64) -> f64 {
    frame.to_f64().unwrap_or(f64::INFINITY) / rate
}
