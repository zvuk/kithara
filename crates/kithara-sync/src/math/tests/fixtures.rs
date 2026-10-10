use std::num::{NonZeroU16, NonZeroU32};

use kithara_beat::{BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid};
use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;
use kithara_warp::SessionBeat;
use num_traits::ToPrimitive;

use crate::{Tempo, TempoStep, TempoTrajectory};

pub(crate) fn host(bpm: f64, beats_per_bar: u16) -> TempoTrajectory {
    TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: Tempo::new(bpm).expect("valid fixture tempo"),
        },
        NonZeroU16::new(beats_per_bar).expect("nonzero fixture meter"),
        NonZeroU32::new(48_000).expect("nonzero fixture rate"),
    )
}

pub(crate) fn position(frames: u64) -> Duration {
    Duration::from_secs_f64(frames.to_f64().expect("fixture frame count fits f64") / 48_000.0)
}

pub(crate) fn grid(
    extent: u64,
    covered: u64,
    beat_frames: u64,
    first: u64,
    meter: Option<(u16, i64)>,
    state: BeatGridState,
) -> BeatGridModel {
    let beats: Vec<_> = (0..=(covered - first) / beat_frames)
        .map(|ordinal| GridBeat {
            confidence: None,
            at: (first + ordinal * beat_frames)
                .to_f64()
                .expect("fixture beat frame fits f64")
                / 48_000.0,
            ordinal: ordinal as i64,
        })
        .collect();
    let meter = meter.map(|(beats_per_bar, origin_beat_ordinal)| Meter {
        beats_per_bar: NonZeroU16::new(beats_per_bar).expect("nonzero fixture meter"),
        origin_beat_ordinal,
    });
    let downbeats = beats
        .iter()
        .filter(|beat| {
            meter.is_some_and(|meter| {
                (beat.ordinal - meter.origin_beat_ordinal)
                    .rem_euclid(i64::from(meter.beats_per_bar.get()))
                    == 0
            })
        })
        .map(|beat| GridDownbeat {
            confidence: None,
            at: beat.at,
            beat_ordinal: beat.ordinal,
        })
        .collect();
    BeatGridModel::try_from(RawBeatGrid {
        state,
        duration: Some(extent.to_f64().expect("fixture extent fits f64") / 48_000.0),
        meter,
        model_id: "sync-contract".into(),
        beats,
        downbeats,
        bpm: 60.0 * 48_000.0 / beat_frames.to_f64().expect("fixture beat span fits f64"),
        schema_version: 1,
        revision: 0,
    })
    .expect("consistent fixture grid")
}

/// The last phase boundary at or before a position: a bar line, or a beat
/// when the grid proves no meter.
pub(crate) fn boundary_at_or_before(grid: &BeatGridModel, position: Duration) -> Option<Duration> {
    let raw = grid.as_raw();
    let metered = raw.meter.is_some();
    raw.downbeats
        .iter()
        .filter(|_| metered)
        .map(|beat| beat.at)
        .chain(raw.beats.iter().filter(|_| !metered).map(|beat| beat.at))
        .take_while(|seconds| *seconds <= position.as_secs_f64())
        .last()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
}
