pub(super) use std::num::{NonZeroU16, NonZeroU32};

pub(super) use kithara_beat::{
    BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid,
};
pub(super) use kithara_platform::time::Duration as Position;
pub(super) use kithara_test_utils::kithara;

use super::*;
pub(super) use crate::{Tempo, TempoStep};

#[kithara::test]
fn entry_family_uses_the_media_offset_and_the_requested_side() {
    let host = TempoTrajectory::new(
        TempoStep {
            frame: SessionFrame::new(0),
            beat: SessionBeat::default(),
            tempo: Tempo::DEFAULT,
        },
        NonZeroU16::new(4).expect("meter"),
        NonZeroU32::new(48_000).expect("rate"),
    );
    let model = BeatGridModel::try_from(RawBeatGrid {
        state: BeatGridState::Final,
        duration: Some(8.0),
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(4).expect("meter"),
            origin_beat_ordinal: 0,
        }),
        model_id: "constant".into(),
        beats: (0..=8)
            .map(|ordinal| GridBeat {
                confidence: None,
                at: ordinal.to_f64().expect("fixture beat ordinal fits f64"),
                ordinal,
            })
            .collect(),
        downbeats: [0, 4, 8]
            .into_iter()
            .map(|beat_ordinal| GridDownbeat {
                confidence: None,
                at: beat_ordinal
                    .to_f64()
                    .expect("fixture downbeat ordinal fits f64"),
                beat_ordinal,
            })
            .collect(),
        bpm: 60.0,
        schema_version: 1,
        revision: 0,
    })
    .expect("consistent grid");
    let grid = model;
    let position = Position::from_secs_f64(0.5);
    assert_eq!(speed(&host, &grid, SessionFrame::new(0)), 2.0);
    assert_eq!(
        entry(
            &host,
            &grid,
            position,
            Bound::AtOrAfter(SessionFrame::new(20_000))
        ),
        Some(SessionFrame::new(108_000))
    );
    assert_eq!(
        entry(
            &host,
            &grid,
            position,
            Bound::AtOrBefore(SessionFrame::new(20_000))
        ),
        Some(SessionFrame::new(12_000))
    );
    assert_eq!(
        entry(
            &host,
            &grid,
            position,
            Bound::AtOrAfter(SessionFrame::new(12_000))
        ),
        Some(SessionFrame::new(12_000))
    );
    assert_eq!(
        phase_error(&host, &grid, position, SessionFrame::new(12_000)).seconds,
        0.0
    );
    assert_eq!(
        jump_target(
            position,
            PhaseError {
                seconds: 0.1,
                period: 4.0,
                beat_seconds: 1.0,
            }
        ),
        Position::from_secs_f64(0.4)
    );
    assert!(
        entry(
            &host,
            &grid,
            Position::from_secs(9),
            Bound::AtOrAfter(SessionFrame::new(0))
        )
        .is_none()
    );
}

#[kithara::test]
fn correction_bounds_every_step_and_cancels_the_phase_integral() {
    let epsilon = 0.001;
    for (from, to, seconds) in [
        (1.0, 1.0, 0.01),
        (1.0, 1.1, -0.01),
        (1.2, 1.0, 0.02),
        (1.0, 1.0, 0.0),
        (1.0, 1.0, 1.01),
    ] {
        let error = PhaseError {
            seconds,
            period: 2.0,
            beat_seconds: 0.5,
        };
        let plan = correction(from, to, error, epsilon);
        let mut previous = from;
        let mut phase = error.shortest();
        for step in &plan.steps {
            assert!((step.speed - previous).abs() <= epsilon);
            assert!(step.seconds >= 0.0 && step.seconds.is_finite());
            phase += (f64::from(step.speed) - f64::from(to)) * step.seconds;
            previous = step.speed;
        }
        assert_eq!(previous, to);
        assert!(phase.abs() < 1e-12);
    }
}
