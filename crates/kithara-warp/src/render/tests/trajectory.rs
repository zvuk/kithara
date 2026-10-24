use std::num::{NonZeroU32, NonZeroU64, NonZeroU128};

use kithara_signal::ExponentialApproach;
use kithara_sync::SpeedCurve;
use kithara_test_utils::{Xorshift64, kithara};

use super::super::trajectory::{Fraction, Trajectory};

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

fn scalar_reduced_fraction(numerator: u128, denominator: u128) -> Option<(u128, NonZeroU128)> {
    let (mut common, mut remainder) = (numerator, denominator);
    while remainder != 0 {
        (common, remainder) = (remainder, common % remainder);
    }
    Some((
        numerator.checked_div(common)?,
        NonZeroU128::new(denominator.checked_div(common)?)?,
    ))
}

fn scalar_lattice_fraction(
    numerator: u128,
    denominator: NonZeroU128,
) -> Option<(u128, NonZeroU128)> {
    let denominator = denominator.get();
    let frames = u64::try_from(numerator / denominator).ok()?;
    let mut remainder = numerator % denominator;
    let mut fraction = 0;
    for _ in 0..32 {
        fraction <<= 1;
        let complement = denominator - remainder;
        if remainder >= complement {
            remainder -= complement;
            fraction += 1;
        } else {
            remainder += remainder;
        }
    }
    if remainder >= denominator - remainder {
        fraction += 1;
    }
    scalar_reduced_fraction(u128::from(frames) * (1u128 << 32) + fraction, 1u128 << 32)
}

#[kithara::test]
fn fraction_reduction_and_lattice_rounding_preserve_scalar_bits_and_bounds() {
    let check = |numerator, denominator| {
        assert_eq!(
            Fraction::new(numerator, denominator).map(|value| (value.numerator, value.denominator)),
            scalar_reduced_fraction(numerator, denominator),
        );
        if let Some(denominator) = NonZeroU128::new(denominator) {
            let value = Fraction {
                numerator,
                denominator,
            };
            assert_eq!(
                value
                    .on_lattice()
                    .map(|value| (value.numerator, value.denominator)),
                scalar_lattice_fraction(numerator, denominator),
                "source fraction {numerator}/{denominator}",
            );
        }
    };
    for denominator in [0, 1, 3, 5, 44_100, u128::from(u64::MAX), u128::MAX] {
        for numerator in [
            0,
            1,
            2,
            3,
            u128::from(u64::MAX),
            u128::from(u64::MAX) + 1,
            u128::MAX,
        ] {
            check(numerator, denominator);
        }
    }
    for bits in 0..u128::BITS {
        let denominator = 1u128 << bits;
        for numerator in [
            0,
            1,
            denominator - 1,
            denominator,
            denominator.saturating_add(1),
            u128::MAX,
        ] {
            check(numerator, denominator);
        }
        for whole in [
            0,
            u128::from(u64::MAX) - 1,
            u128::from(u64::MAX),
            u128::from(u64::MAX) + 1,
        ] {
            if let Some(origin) = whole.checked_mul(denominator) {
                for fraction in [0, denominator / 2, denominator - 1] {
                    if let Some(numerator) = origin.checked_add(fraction) {
                        check(numerator, denominator);
                    }
                }
            }
        }
        if bits > 32 {
            let half = 1u128 << (bits - 33);
            for numerator in [half - 1, half, half + 1] {
                check(numerator, denominator);
            }
        }
    }
    let mut random = Xorshift64::new(0x1234_5678_abcd_ef01);
    for bits in 0..4_096 {
        let numerator = (u128::from(random.next_u64()) << 64) | u128::from(random.next_u64());
        let denominator = if bits % 2 == 0 {
            1u128 << (bits % 128)
        } else {
            (u128::from(random.next_u64()) << 64) | u128::from(random.next_u64()) | 1
        };
        check(numerator, denominator);
    }
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

#[kithara::test]
fn an_approach_keeps_one_mapping_basis_across_quantum_partitions() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach");
    let mut initial = Trajectory::new(1.0);
    initial
        .replace(SpeedCurve::Approach(curve))
        .expect("accepted approach");
    let whole = initial.span(1234, rate, 2048).expect("whole approach");
    for quantum in [1, 32, 127, 512] {
        let mut trajectory = initial.clone();
        let mut joined: Option<kithara_signal::SourceSpan> = None;
        let mut cursor = 0;
        while cursor < 2048 {
            let frames = quantum.min(2048 - cursor);
            let span = trajectory.span(1234, rate, frames).expect("partition");
            for offset in 0..=span.output_frames() {
                assert_eq!(
                    span.source_ratio_at(offset),
                    whole.source_ratio_at(u64::try_from(cursor).expect("cursor") + offset)
                );
            }
            joined = Some(joined.map_or(span, |previous| {
                previous.followed_by(span).expect("one mapping basis joins")
            }));
            trajectory.advance(span).expect("partition consumed");
            cursor += frames;
        }
        assert_eq!(joined, Some(whole));
    }
    let boundary = 1u64 << 63;
    let fixed = ExponentialApproach::new(1.1, 1.1, 220.5).expect("fixed analytic law");
    for (curve, speed) in [
        (SpeedCurve::Constant(1.1), f64::from(1.1f32)),
        (
            SpeedCurve::Ramp {
                to: 1.1,
                frames: NonZeroU64::new(97).expect("ramp"),
            },
            f64::from(1.1f32),
        ),
        (SpeedCurve::Approach(fixed), 1.1),
        (
            SpeedCurve::ApproachSteps(vec![(0, fixed), (97, fixed)].into()),
            1.1,
        ),
    ] {
        let mut trajectory = Trajectory::new(1.0);
        trajectory.replace(curve).expect("accepted wide source law");
        trajectory
            .advance_by(0, rate, 97)
            .expect("established native law phase");
        let origin = trajectory
            .at_offset(0, rate, 0)
            .expect("established source phase");
        let reference = kithara_signal::SourceSpan::approaching(
            origin.0,
            origin.1,
            ExponentialApproach::new(speed, speed, 220.5).expect("independent steady law"),
            0,
            rate,
            boundary,
        )
        .expect("the physical endpoint fits the source clock");
        let endpoint = reference
            .source_ratio_at(boundary)
            .expect("exact large endpoint");
        assert_eq!(
            trajectory
                .first_at_or_after(0, rate, endpoint, None)
                .expect("search brackets must not overshoot the representable source range"),
            Some(boundary)
        );
        assert_eq!(
            trajectory
                .first_at_or_after(0, rate, endpoint, Some(boundary - 1))
                .expect("the previous exact frame remains before EOF"),
            None
        );
    }
}

#[kithara::test]
fn a_rounded_unit_speed_does_not_finish_an_approach() {
    let curve =
        ExponentialApproach::new(1.0 - 2.0_f64.powi(-26), 1.0, 220.5).expect("approach to unity");
    let mut trajectory = Trajectory::new(1.0);
    trajectory
        .replace(SpeedCurve::Approach(curve))
        .expect("approach accepted");
    assert_eq!(trajectory.speed().expect("observable speed"), 1.0);
    assert!(!trajectory.unity_interval());
    assert!(!trajectory.constant_unity());
    assert!(!trajectory.passthrough_speed(1.0));
}

#[kithara::test]
fn an_approach_replacement_keeps_its_reached_rate_and_q32_source_phase() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    let first = ExponentialApproach::new(1.0, 0.9, 220.5).expect("initial approach");
    let mut trajectory = Trajectory::new(1.0);
    trajectory
        .replace(SpeedCurve::Approach(first))
        .expect("initial admission");
    let span = trajectory
        .span(10_000, rate, 97)
        .expect("before replacement");
    let (numerator, denominator) = span.source_ratio_at(97).expect("consumed position");
    let carried = Fraction {
        numerator,
        denominator,
    }
    .on_lattice()
    .expect("Q32 origin");
    trajectory.advance(span).expect("consumed prefix");
    let reached = first.rate_at(97.0).expect("reached speed");
    let second = ExponentialApproach::new(reached, 1.1, 220.5).expect("new target");
    trajectory
        .replace(SpeedCurve::Approach(second))
        .expect("replacement admission");
    let span = trajectory.span(0, rate, 128).expect("replacement span");
    assert_eq!(
        span.source_ratio_at(0),
        Some((carried.numerator, carried.denominator))
    );
    let whole = trajectory.span(0, rate, 256).expect("whole replacement");
    trajectory.advance(span).expect("replacement prefix");
    let next = trajectory.span(0, rate, 128).expect("replacement suffix");
    assert_eq!(span.followed_by(next), Some(whole));
}

#[kithara::test]
fn a_source_discontinuity_rebases_an_approach_at_its_reached_rate() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    let first = ExponentialApproach::new(1.0, 0.9, 220.5).expect("approach");
    let mut trajectory = Trajectory::new(1.0);
    trajectory
        .replace(SpeedCurve::Approach(first))
        .expect("accepted approach");
    let before = trajectory
        .span(10_000, rate, 97)
        .expect("before discontinuity");
    trajectory.advance(before).expect("consumed prefix");
    let reached = trajectory.speed().expect("reached speed");
    trajectory.reset();
    assert_eq!(trajectory.speed().expect("rebased speed"), reached);
    let position = (203, NonZeroU128::new(4).expect("fractional source phase"));
    trajectory
        .reanchor(50, rate, position)
        .expect("exact postseek phase");
    let after = trajectory.span(50, rate, 128).expect("after backward seek");
    assert_eq!(after.source_ratio_at(0), Some(position));
    let expected_curve = ExponentialApproach::new(
        first.rate_at(97.0).expect("independent reached rate"),
        first.target(),
        first.time_constant(),
    )
    .expect("remaining approach");
    let expected = kithara_signal::SourceSpan::approaching(
        position.0,
        position.1,
        expected_curve,
        0,
        rate,
        256,
    )
    .expect("expected rebased mapping");
    trajectory.advance(after).expect("rebased prefix");
    let next = trajectory.span(50, rate, 128).expect("rebased suffix");
    assert_eq!(after.followed_by(next), Some(expected));
}

#[kithara::test]
fn analytic_correction_segments_preserve_positions_and_forecast_each_boundary() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    let first = ExponentialApproach::new(1.0, 0.9, 220.5).expect("base approach");
    let second = ExponentialApproach::new(
        first.rate_at(97.0).expect("reached base rate") + 0.001,
        0.901,
        220.5,
    )
    .expect("bounded correction approach");
    let third = ExponentialApproach::new(
        second.rate_at(204.0).expect("reached correction rate") - 0.001,
        0.9,
        220.5,
    )
    .expect("correction ends");
    let first_span =
        kithara_signal::SourceSpan::approaching(500, NonZeroU128::MIN, first, 0, rate, 97)
            .expect("first reference law");
    let first_end = first_span.source_ratio_at(97).expect("first boundary");
    let second_span =
        kithara_signal::SourceSpan::approaching(first_end.0, first_end.1, second, 0, rate, 204)
            .expect("second reference law");
    let second_end = second_span.source_ratio_at(204).expect("second boundary");
    let third_span =
        kithara_signal::SourceSpan::approaching(second_end.0, second_end.1, third, 0, rate, 723)
            .expect("third reference law");
    let expected = |frame: u64| {
        if frame <= 97 {
            first_span.source_ratio_at(frame)
        } else if frame <= 301 {
            second_span.source_ratio_at(frame - 97)
        } else {
            third_span.source_ratio_at(frame - 301)
        }
    };
    for quantum in [1, 32, 127, 512] {
        let mut trajectory = Trajectory::new(1.0);
        trajectory
            .replace(SpeedCurve::ApproachSteps(
                vec![(0, first), (97, second), (301, third)].into(),
            ))
            .expect("finite correction segments accepted");
        assert_eq!(
            trajectory
                .at_offset(500, rate, 700)
                .expect("forecast crosses real segment boundaries"),
            expected(700).expect("forecast reference")
        );
        for boundary in [0, 1, 96, 97, 98, 300, 301, 302, 700, 1_024] {
            let endpoint = expected(boundary).expect("independent span endpoint");
            assert_eq!(
                trajectory
                    .first_at_or_after(500, rate, endpoint, None)
                    .expect("projected EOF across analytic segments"),
                Some(boundary),
            );
            assert_eq!(
                trajectory
                    .first_at_or_after(500, rate, endpoint, Some(boundary))
                    .expect("inclusive exact segment endpoint"),
                Some(boundary),
            );
            if boundary > 0 {
                assert_eq!(
                    trajectory
                        .first_at_or_after(500, rate, endpoint, Some(boundary - 1))
                        .expect("one frame before exact segment endpoint"),
                    None,
                );
            }
        }

        let mut cursor = 0;
        while cursor < 1024 {
            let span = trajectory
                .span(500, rate, quantum.min(1024 - cursor))
                .expect("segment quantum");
            for offset in 0..=span.output_frames() {
                assert_eq!(
                    span.source_ratio_at(offset),
                    expected(u64::try_from(cursor).expect("cursor") + offset)
                );
            }
            cursor += usize::try_from(span.output_frames()).expect("quantum length");
            trajectory.advance(span).expect("physical segment consumed");
        }
    }
}

#[kithara::test]
fn a_source_discontinuity_preserves_future_analytic_correction_boundaries() {
    let rate = NonZeroU32::new(44_100).expect("sample rate");
    let first = ExponentialApproach::new(1.0, 0.9, 220.5).expect("first law");
    let second = ExponentialApproach::new(1.1, 0.9, 220.5).expect("second law");
    let mut trajectory = Trajectory::new(1.0);
    trajectory
        .replace(SpeedCurve::ApproachSteps(
            vec![(0, first), (97, second)].into(),
        ))
        .expect("two analytic segments");
    let prefix = trajectory.span(500, rate, 64).expect("prefix before seek");
    trajectory.advance(prefix).expect("prefix consumed");
    let reached = trajectory.speed().expect("reached speed");
    trajectory.reset();
    assert_eq!(
        trajectory
            .speed()
            .expect("source rebase retains tempo clock"),
        reached
    );
    let position = (203, NonZeroU128::new(4).expect("fractional source phase"));
    trajectory
        .reanchor(50, rate, position)
        .expect("exact postseek phase");
    let after = trajectory
        .span(50, rate, 128)
        .expect("new source waits for the original boundary");
    assert_eq!(after.source_ratio_at(0), Some(position));
    assert_eq!(after.output_frames(), 33);
    trajectory
        .advance(after)
        .expect("reached existing correction boundary");
    assert_eq!(trajectory.speed().expect("next segment rate"), 1.1);
    let next = trajectory
        .span(50, rate, 128)
        .expect("next law starts at actual consumed source");
    assert_eq!(next.source_ratio_at(0), after.source_ratio_at(33));
}

#[kithara::test]
fn invalid_analytic_segment_order_rejects_without_replacing_the_speed() {
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("valid law");
    let mut trajectory = Trajectory::new(1.0);
    for steps in [
        vec![],
        vec![(1, curve)],
        vec![(0, curve), (0, curve)],
        vec![(0, curve), (2, curve), (1, curve)],
    ] {
        assert!(
            trajectory
                .replace(SpeedCurve::ApproachSteps(steps.into()))
                .is_err()
        );
        assert_eq!(trajectory.speed().expect("unchanged speed"), 1.0);
        assert!(trajectory.constant_unity());
    }
}
