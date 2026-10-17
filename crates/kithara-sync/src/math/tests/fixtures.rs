use std::num::{NonZeroU16, NonZeroU32};

use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;
use num_traits::ToPrimitive;

use crate::{Meter, SessionBeat, Tempo, TempoStep, TempoTrajectory, TrackBeat, TrackGrid};

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
) -> TrackGrid {
    let beats: Vec<_> = (0..=(covered - first) / beat_frames)
        .map(|ordinal| TrackBeat {
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
        .copied()
        .filter(|beat| {
            meter.is_some_and(|meter| {
                (beat.ordinal - meter.origin_beat_ordinal)
                    .rem_euclid(i64::from(meter.beats_per_bar.get()))
                    == 0
            })
        })
        .collect();
    TrackGrid::new(
        beats,
        downbeats,
        meter,
        60.0 * 48_000.0 / beat_frames.to_f64().expect("fixture beat span fits f64"),
        Some(extent.to_f64().expect("fixture extent fits f64") / 48_000.0),
    )
    .expect("consistent fixture grid")
}

/// Last listed bar line or unmetered beat at or before a position.
pub(crate) fn boundary_at_or_before(grid: &TrackGrid, position: Duration) -> Option<Duration> {
    let beats = if grid.meter().is_some() {
        grid.downbeats()
    } else {
        grid.beats()
    };
    beats
        .iter()
        .take_while(|beat| beat.at <= position.as_secs_f64())
        .last()
        .and_then(|beat| Duration::try_from_secs_f64(beat.at).ok())
}

pub(crate) fn source_grid(model: &TrackGrid) -> crate::SourceGrid {
    crate::SourceGrid::new(
        model,
        NonZeroU32::new(48_000).expect("fixture rate"),
        None,
        false,
    )
    .expect("served source geometry")
}
