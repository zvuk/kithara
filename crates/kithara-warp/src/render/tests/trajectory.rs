use std::num::{NonZeroU32, NonZeroU64, NonZeroU128};

use kithara_test_utils::kithara;

use super::super::trajectory::{Fraction, Trajectory};
use crate::SpeedCurve;

fn interrupted_ramps() -> Vec<(u128, NonZeroU128)> {
    let mut trajectory = Trajectory::new(1.0);
    let rate = NonZeroU32::new(192_000).expect("DJ-session source rate");
    let initial = trajectory
        .span(6_912_000_000, rate, 480_000)
        .expect("ten-hour position");
    trajectory.advance(initial).expect("initial advance");
    let lengths = [47_999, 48_000, 360_000, 2_880_000, 123_457];
    let targets = [1.02, 0.99, 1.01, 0.05, 4.0];
    let mut positions = Vec::new();
    for index in 0..1_000 {
        let length = lengths[index % lengths.len()];
        let before = trajectory
            .span(0, rate, 1)
            .expect("current phase")
            .source_ratio_at(0)
            .expect("phase");
        trajectory
            .replace(SpeedCurve::Ramp {
                to: targets[index % targets.len()],
                frames: NonZeroU64::new(length).expect("duration"),
            })
            .expect("every replacement is admitted");
        let after = trajectory
            .span(0, rate, 1)
            .expect("replacement phase")
            .source_ratio_at(0)
            .expect("phase");
        let before = Fraction {
            numerator: before.0,
            denominator: before.1,
        };
        let rounded = before.on_lattice().expect("bounded origin");
        let actual = Fraction {
            numerator: after.0,
            denominator: after.1,
        };
        let common = rounded.common(actual).expect("same lattice");
        assert_eq!(rounded.at(common), actual.at(common));
        let common = before.common(actual).expect("rounding comparison");
        let distance = before
            .at(common)
            .expect("before")
            .abs_diff(actual.at(common).expect("after"));
        assert!(
            distance * (1u128 << 33) <= common,
            "at most half a Q32 unit"
        );
        let interruption = (length / 2) | 1;
        let span = trajectory
            .span(0, rate, usize::try_from(interruption).expect("frames"))
            .expect("odd interruption");
        let endpoint = span.source_ratio_at(interruption).expect("landing");
        assert!(endpoint.1.get() <= (1u128 << 32) * u128::from(length));
        trajectory
            .advance(span)
            .expect("advance never accumulates history factors");
        positions.push(endpoint);
    }
    positions
}

#[kithara::test]
fn a_thousand_interrupted_ramps_keep_bounded_phase_and_are_bit_identical() {
    let first = interrupted_ramps();
    let second = interrupted_ramps();
    assert_eq!(first.len(), 1_000);
    assert_eq!(first, second);
}

#[kithara::test]
fn replacement_rounds_half_a_phase_unit_toward_the_next_source_frame() {
    let half = Fraction::new(1, 1u128 << 33).expect("half unit");
    let rounded = half.on_lattice().expect("rounded unit");
    assert_eq!(rounded.numerator, 1);
    assert_eq!(rounded.denominator.get(), 1u128 << 32);
}

#[kithara::test]
#[case::below_half(0.25, 0)]
#[case::half(0.5, 1)]
#[case::above_half(0.75, 1)]
fn identity_snap_rounds_to_the_nearest_source_frame(#[case] speed: f32, #[case] rounded: u64) {
    let mut trajectory = Trajectory::new(speed);
    let span = trajectory
        .span(0, NonZeroU32::MIN, 1)
        .expect("fractional source");
    trajectory.advance(span).expect("advance");
    trajectory.snap_to_frame().expect("whole-frame snap");
    assert_eq!(
        trajectory
            .span(0, NonZeroU32::MIN, 1)
            .expect("identity origin")
            .source_ratio_at(0),
        Some((u128::from(rounded), NonZeroU128::MIN)),
        "nearest whole frame; exact ties advance to the next frame"
    );
}
