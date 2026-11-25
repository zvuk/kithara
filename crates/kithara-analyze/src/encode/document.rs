use std::{
    io::{Error, ErrorKind},
    ops::Range,
};

use kithara::analysis::{
    AnalysisOutcome, BeatGridModel, BeatSnapshot, BeatState, Mode, RawBeatGrid, TrackAnalysis,
};
use serde::Serialize;

use crate::{RunError, analysis::OpenError, input::Track};

pub(crate) fn json(
    track: &Track,
    result: &Result<AnalysisOutcome, OpenError>,
) -> Result<Vec<u8>, RunError> {
    match result {
        Ok(AnalysisOutcome::Complete(progress)) => settled(track, progress.analysis(), "complete"),
        Ok(AnalysisOutcome::Incomplete(progress)) => {
            settled(track, progress.analysis(), "incomplete")
        }
        Ok(AnalysisOutcome::Failed { error, .. }) => {
            failure(track, "failed", Some(error.to_string()))
        }
        Ok(AnalysisOutcome::Cancelled { .. }) => failure(track, "cancelled", None),
        Err(error) => failure(track, "failed", Some(error.to_string())),
        Ok(_) => Err(Error::new(ErrorKind::Unsupported, "unknown analysis outcome").into()),
    }
}

fn failure(
    track: &Track,
    status: &'static str,
    error: Option<String>,
) -> Result<Vec<u8>, RunError> {
    Ok(serde_json::to_vec(&Failure {
        status,
        error,
        id: &track.id,
        src: &track.src,
    })?)
}

fn settled(
    track: &Track,
    analysis: &TrackAnalysis,
    status: &'static str,
) -> Result<Vec<u8>, RunError> {
    let fingerprint = analysis.fingerprint();
    let (grid, grid_unavailable) = match BeatGridModel::try_from(analysis) {
        Ok(grid) => (Some(RawBeatGrid::from(grid)), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let document = Document {
        status,
        grid,
        grid_unavailable,
        id: &track.id,
        src: &track.src,
        tool_version: env!("CARGO_PKG_VERSION"),
        missing: analysis.missing().iter().map(range_pair).collect(),
        fingerprint: Fingerprint {
            waveform: fingerprint.waveform(),
            beat: fingerprint.beat(),
            key: fingerprint.key(),
        },
        source: Source {
            sample_rate: analysis.source_sample_rate().get(),
            media_origin_s: analysis.media_origin().map(|origin| origin.as_secs_f64()),
            extent_frames: analysis.extent(),
        },
        coverage: analysis.coverage().iter().map(range_pair).collect(),
        beat: analysis.beat().map(Beat::try_from).transpose()?,
        key: analysis.key().map(|estimate| Key {
            tonic: estimate.key().tonic.semitone(),
            mode: estimate.key().mode,
            strength: estimate.strength(),
        }),
        waveform: analysis.waveform().map(|waveform| Wave {
            bands: ["low", "low_mid", "high_mid", "high"],
            span_frames: waveform.span_frames().get(),
            buckets: waveform
                .buckets()
                .map(|bucket| [bucket.left(), bucket.right()])
                .collect(),
        }),
    };
    Ok(serde_json::to_vec(&document)?)
}

fn range_pair(range: &Range<u64>) -> [u64; 2] {
    [range.start, range.end]
}

#[derive(Serialize)]
struct Failure<'a> {
    id: &'a str,
    src: &'a str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Document<'a> {
    id: &'a str,
    src: &'a str,
    status: &'static str,
    tool_version: &'static str,
    fingerprint: Fingerprint<'a>,
    beat: Option<Beat>,
    grid: Option<RawBeatGrid>,
    grid_unavailable: Option<String>,
    key: Option<Key>,
    waveform: Option<Wave>,
    source: Source,
    coverage: Vec<[u64; 2]>,
    missing: Vec<[u64; 2]>,
}

#[derive(Serialize)]
struct Fingerprint<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    beat: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    waveform: Option<&'a str>,
}

#[derive(Serialize)]
struct Source {
    extent_frames: Option<u64>,
    media_origin_s: Option<f64>,
    sample_rate: u32,
}

#[derive(Serialize)]
struct Beat {
    state: &'static str,
    confidence: Option<f32>,
    beats: Vec<Marker>,
    downbeats: Vec<Marker>,
    unanalysed: Vec<[u64; 2]>,
    bpm: f64,
}

impl TryFrom<&BeatSnapshot> for Beat {
    type Error = RunError;

    fn try_from(snapshot: &BeatSnapshot) -> Result<Self, Self::Error> {
        let artifact = snapshot.artifact();
        let state = match snapshot.state() {
            BeatState::Final => "final",
            BeatState::Provisional => "provisional",
            _ => {
                return Err(Error::new(ErrorKind::Unsupported, "unknown beat state").into());
            }
        };
        Ok(Self {
            state,
            bpm: artifact.bpm(),
            confidence: snapshot.confidence(),
            unanalysed: snapshot.unanalysed().iter().map(range_pair).collect(),
            beats: markers(artifact.beats(), artifact.beat_confidence()),
            downbeats: markers(artifact.downbeats(), artifact.downbeat_confidence()),
        })
    }
}

#[derive(Serialize)]
struct Marker {
    confidence: Option<f32>,
    frame: u64,
}

fn markers(frames: &[u64], confidences: &[Option<f32>]) -> Vec<Marker> {
    frames
        .iter()
        .zip(confidences)
        .map(|(&frame, &confidence)| Marker { confidence, frame })
        .collect()
}

#[derive(Serialize)]
struct Key {
    mode: Mode,
    strength: f32,
    tonic: u8,
}

#[derive(Serialize)]
struct Wave {
    buckets: Vec<[[f32; 4]; 2]>,
    bands: [&'static str; 4],
    span_frames: u64,
}
