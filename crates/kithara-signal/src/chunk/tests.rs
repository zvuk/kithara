impl SourceSpan {
    fn from_rational(
        start: u128,
        step: u128,
        denominator: NonZeroU128,
        sample_rate: NonZeroU32,
        output_frames: u64,
    ) -> Option<Self> {
        Self::try_from((start, step, denominator, sample_rate, output_frames)).ok()
    }

    fn from_ramp(
        start: u128,
        step: u128,
        step_change: i128,
        denominator: NonZeroU128,
        sample_rate: NonZeroU32,
        output_frames: u64,
    ) -> Option<Self> {
        Self::try_from((
            start,
            step,
            step_change,
            denominator,
            sample_rate,
            output_frames,
        ))
        .ok()
    }
}

use std::{
    num::{NonZeroU32, NonZeroU128},
    ops::Range,
};

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::SourceSpan;
use crate::ExponentialApproach;

#[kithara::test]
fn seconds_retain_the_exact_source_point_instead_of_truncated_nanos() {
    let frames = 431_797;
    let rate = NonZeroU32::new(44_100).expect("rate");
    let point = SourceSpan::new(frames, frames + 1, rate, 1)
        .and_then(|span| span.for_output_range(0..0))
        .expect("source point");
    assert_eq!(
        point.seconds_at(0),
        Some(frames.to_f64().expect("frames") / f64::from(rate.get()))
    );
    assert_ne!(
        point.seconds_at(0),
        point.position_at(0).map(|position| position.as_secs_f64())
    );
    assert_eq!(point.seconds_at(1), None);
}

#[kithara::test]
fn empty_source_mapping_slices_join_without_underflow() {
    let span = SourceSpan::from_rational(
        1,
        2,
        NonZeroU128::new(2).expect("denominator"),
        NonZeroU32::new(48_000).expect("rate"),
        8,
    )
    .expect("span");
    let empty = span.for_output_range(3..3).expect("empty slice");
    assert_eq!(empty.followed_by(empty), Some(empty));
    assert_eq!(empty.position_at(0), span.position_at(3));
    let suffix = span.for_output_range(3..8).expect("suffix");
    assert_eq!(empty.followed_by(suffix), Some(suffix));
    assert_eq!(
        empty.output_ratio_at_source(3, (9, NonZeroU128::new(2).expect("target denominator"))),
        Some((4, NonZeroU128::MIN))
    );
    let physical = SourceSpan::try_from((
        13,
        2,
        NonZeroU128::new(4).expect("physical denominator"),
        span.sample_rate(),
        1,
    ))
    .expect("one fractional physical output");
    let published = empty
        .map_output_span(3, physical)
        .expect("empty affine input keeps its source advance");
    assert_eq!(
        published.source_ratio_at(0),
        Some((15, NonZeroU128::new(4).expect("published denominator")))
    );
    assert_eq!(
        published.source_ratio_at(1),
        Some((17, NonZeroU128::new(4).expect("published denominator")))
    );
    let declining = SourceSpan::try_from((0, 1, -2, NonZeroU128::MIN, span.sample_rate(), 1))
        .expect("one valid declining source advance");
    let endpoint = declining
        .for_output_range(1..1)
        .expect("empty ramp endpoint");
    assert_eq!(endpoint.position_at(0), declining.position_at(1));
}

#[kithara::test]
fn fractional_start_survives_a_change_to_an_integer_slope() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let span = SourceSpan::from_rational(1, 2, NonZeroU128::new(2).expect("denominator"), rate, 8)
        .expect("fractional source origin");
    assert_eq!(span.position_at(0), Some(Duration::from_nanos(10_416)));
    assert_eq!(span.position_at(3), Some(Duration::from_nanos(72_916)));
    assert_eq!(
        span.source_ratio_at(3),
        Some((7, NonZeroU128::new(2).expect("denominator")))
    );
    assert_eq!(
        span.for_output_range(3..8).expect("suffix").position_at(0),
        span.position_at(3)
    );
}

#[kithara::test]
fn rational_source_mapping_checks_its_entire_range() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    assert!(SourceSpan::from_rational(u128::MAX, 1, NonZeroU128::MIN, rate, 1).is_none());
    assert!(
        SourceSpan::from_rational(u128::from(u64::MAX), 1, NonZeroU128::MIN, rate, 1).is_none()
    );
    assert!(SourceSpan::from_rational(0, 1, NonZeroU128::MIN, rate, 0).is_none());
    let standing =
        SourceSpan::from_rational(1, 0, NonZeroU128::new(2).expect("denominator"), rate, 8)
            .expect("standing fractional position");
    assert_eq!(standing.position_at(8), standing.position_at(0));
}

#[kithara::test]
fn ramp_source_mapping_slices_the_analytic_integral() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let span = SourceSpan::from_ramp(
        0,
        33,
        2,
        NonZeroU128::new(64).expect("denominator"),
        rate,
        32,
    )
    .expect("positive ramp");
    assert_eq!(span.source_ratio_at(32), Some((32, NonZeroU128::MIN)));
    let first = span.for_output_range(0..7).expect("prefix");
    let second = span.for_output_range(7..32).expect("suffix");
    assert_eq!(second.position_at(0), span.position_at(7));
    assert_eq!(first.followed_by(second), Some(span));
    assert!(SourceSpan::from_ramp(0, 1, -2, NonZeroU128::MIN, rate, 2).is_none());
}

#[kithara::test]
fn wide_source_mapping_keeps_exact_positions_slices_and_timestamps() {
    let rate = NonZeroU32::new(192_000).expect("rate");
    let denominator = NonZeroU128::new(1u128 << 90).expect("wide denominator");
    let origin = 6_912_000_000u128;
    let span = SourceSpan::from_ramp(
        origin * denominator.get() + 1,
        denominator.get(),
        1,
        denominator,
        rate,
        32,
    )
    .expect("wide mapping");
    assert_eq!(
        span.source_ratio_at(0),
        Some((origin * denominator.get() + 1, denominator))
    );
    assert_eq!(span.position_at(0), Some(Duration::from_secs(36_000)));
    assert_eq!(span.position_at(1), Some(Duration::new(36_000, 5_208)));
    let prefix = span.for_output_range(0..7).expect("prefix");
    let suffix = span.for_output_range(7..32).expect("suffix");
    assert_eq!(suffix.source_ratio_at(0), span.source_ratio_at(7));
    assert_eq!(prefix.followed_by(suffix), Some(span));
}

#[kithara::test]
fn output_positions_retain_fractional_source_phase() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let span = SourceSpan::new(100, 292, rate, 128).expect("span");
    let sliced = span.for_output_range(1..127).expect("slice");
    assert_eq!(span.position_at(1), Some(Duration::from_nanos(2_114_583)));
    assert_eq!(sliced.position_at(1), span.position_at(2));
    assert_eq!(span.position_at(129), None);
    let large = SourceSpan::new(u64::MAX - 192, u64::MAX, rate, 128).expect("span");
    assert!(large.position_at(127).is_some());
}

#[kithara::test]
fn nested_source_slices_keep_the_original_rational_phase() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    for origin in [0, 100, u64::MAX - 192] {
        let span = SourceSpan::new(origin, origin + 192, rate, 128).expect("span");
        let nested = span
            .for_output_range(0..127)
            .expect("prefix")
            .for_output_range(0..2)
            .expect("nested prefix");
        assert_eq!(nested.end(), origin + 3);
        assert_eq!(Some(nested), span.for_output_range(0..2));
        let first = span.for_output_range(0..1).expect("first");
        let rest = span.for_output_range(1..128).expect("rest");
        assert_eq!(first.followed_by(rest), Some(span));
    }
}

#[kithara::test]
fn rounded_endpoints_do_not_authorize_a_different_mapping_join() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    let original = SourceSpan::new(0, 192, rate, 128).expect("span");
    let first = original.for_output_range(0..1).expect("first");
    let rounded = SourceSpan::new(1, 4, rate, 2).expect("same slope, wrong phase");
    assert!(first.followed_by(rounded).is_none());
}
#[kithara::test]
fn source_span_requires_output_and_preserves_valid_source_boundaries() {
    let rate = NonZeroU32::new(48_000).expect("rate");
    assert!(SourceSpan::new(0, 0, rate, 0).is_none());
    assert!(SourceSpan::new(0, 1, rate, 0).is_none());
    assert!(SourceSpan::new(2, 1, rate, 1).is_none());
    let standing = SourceSpan::new(u64::MAX, u64::MAX, rate, 128).expect("standing source");
    assert_eq!(standing.start(), u64::MAX);
    assert_eq!(standing.end(), u64::MAX);
    assert_eq!(
        standing.for_output_range(127..128).expect("suffix").end(),
        u64::MAX
    );
    assert!(standing.for_output_range(0..129).is_none());
    assert!(
        standing
            .for_output_range(Range { start: 2, end: 1 })
            .is_none()
    );
    let full = SourceSpan::new(0, u64::MAX, rate, u64::MAX).expect("full source");
    assert_eq!(full.end(), u64::MAX);
    assert_eq!(
        full.for_output_range(1..u64::MAX).expect("suffix").start(),
        1
    );
}

#[kithara::test]
fn exponential_source_spans_keep_nested_slices_and_adjacent_quantum_bases() {
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("law");
    let rate = NonZeroU32::new(44_100).expect("rate");
    let origin = NonZeroU128::new(3).expect("denominator");
    let whole = SourceSpan::approaching(1, origin, curve, 0, rate, 4_096).expect("whole");
    let nested = whole
        .for_output_range(31..1_023)
        .expect("outer")
        .for_output_range(7..92)
        .expect("nested");
    assert_eq!(Some(nested), whole.for_output_range(38..123));
    for frame in 0..=nested.output_frames() {
        assert_eq!(
            nested.source_ratio_at(frame),
            whole.source_ratio_at(38 + frame)
        );
    }
    let mut joined = SourceSpan::approaching(1, origin, curve, 0, rate, 64).expect("first quantum");
    for elapsed in (64..4_096).step_by(64) {
        let next =
            SourceSpan::approaching(1, origin, curve, elapsed, rate, 64).expect("next quantum");
        joined = joined.followed_by(next).expect("same original basis");
    }
    assert_eq!(joined, whole);
    assert!(whole.source_ratio_at(4_097).is_none());
}

#[kithara::test]
fn exponential_source_spans_keep_adjacent_frames_above_f64_integer_precision() {
    let curve = ExponentialApproach::new(1.75, 1.25, 220.5).expect("law");
    let rate = NonZeroU32::new(44_100).expect("rate");
    let elapsed = (1u64 << 53) + 3;
    let span = SourceSpan::approaching(
        1,
        NonZeroU128::new(3).expect("denominator"),
        curve,
        elapsed,
        rate,
        2,
    )
    .expect("large elapsed");
    let first = (u128::from(elapsed) * 5 + 441) / 4;
    assert_eq!(
        span.source_ratio_at(0),
        Some((first * 3 + 1, NonZeroU128::new(3).expect("third")))
    );
    assert_eq!(
        span.source_ratio_at(1),
        Some((first * 12 + 19, NonZeroU128::new(12).expect("twelfth")))
    );
    assert_eq!(
        span.source_ratio_at(2),
        Some((first * 6 + 17, NonZeroU128::new(6).expect("sixth")))
    );
    let next = span.for_output_range(1..2).expect("one frame");
    assert_eq!(next.source_ratio_at(0), span.source_ratio_at(1));
    assert_eq!(next.end() - next.start(), 1);
}

#[kithara::test]
fn an_exponential_join_rejects_a_different_law_or_original_origin() {
    let rate = NonZeroU32::new(44_100).expect("rate");
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("law");
    let first = SourceSpan::approaching(0, NonZeroU128::MIN, curve, 0, rate, 16).expect("first");
    let other_origin =
        SourceSpan::approaching(1, NonZeroU128::MIN, curve, 16, rate, 16).expect("other origin");
    assert!(first.followed_by(other_origin).is_none());
    let other_curve = ExponentialApproach::new(1.0, 0.9, 221.5).expect("other law");
    let next = SourceSpan::approaching(0, NonZeroU128::MIN, other_curve, 16, rate, 16)
        .expect("different law");
    assert!(first.followed_by(next).is_none());
    let empty = first.for_output_range(7..7).expect("point");
    assert_eq!(empty.followed_by(empty), Some(empty));
}

#[kithara::test]
fn affine_exponential_maps_preserve_one_transform_across_quantum_partitions() {
    let rate = NonZeroU32::new(44_100).expect("rate");
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("law");
    let denominator = NonZeroU128::new(3).expect("denominator");
    let pivot = (1, denominator);
    let position = (1001, NonZeroU128::new(7).expect("position denominator"));
    let scale = (2, denominator);
    let whole = SourceSpan::approaching(1, denominator, curve, 0, rate, 128)
        .expect("whole")
        .scaled_from(pivot, position, scale)
        .expect("affine map");
    assert_eq!(whole.source_ratio_at(0), Some((143, NonZeroU128::MIN)));
    let first = SourceSpan::approaching(1, denominator, curve, 0, rate, 64)
        .expect("first")
        .scaled_from(pivot, position, scale)
        .expect("same affine map");
    let second = SourceSpan::approaching(1, denominator, curve, 64, rate, 64)
        .expect("second")
        .scaled_from(pivot, position, scale)
        .expect("same affine map");
    assert_eq!(first.followed_by(second), Some(whole));
    let slice = whole.for_output_range(7..91).expect("slice");
    assert_eq!(slice.source_ratio_at(33), whole.source_ratio_at(40));
    let translated = whole
        .scaled_from(
            position,
            (7, NonZeroU128::MIN),
            (1, NonZeroU128::new(2).expect("half")),
        )
        .expect("second transform");
    assert_eq!(translated.source_ratio_at(0), Some((7, NonZeroU128::MIN)));
    assert_eq!(
        translated
            .for_output_range(0..64)
            .expect("first")
            .followed_by(translated.for_output_range(64..128).expect("second")),
        Some(translated)
    );
}

#[kithara::test]
fn affine_rational_maps_keep_exact_ramp_coefficients_and_negative_intercepts() {
    let rate = NonZeroU32::new(44_100).expect("rate");
    let third = NonZeroU128::new(3).expect("third");
    let span = SourceSpan::from_ramp(100, 5, -1, third, rate, 4).expect("ramp");
    let transformed = span
        .scaled_from(
            (100, third),
            (2, NonZeroU128::new(5).expect("fifth")),
            (2, NonZeroU128::new(7).expect("seventh")),
        )
        .expect("affine ramp");
    let expected = SourceSpan::from_ramp(
        42,
        50,
        -10,
        NonZeroU128::new(105).expect("common basis"),
        rate,
        4,
    )
    .expect("exact transformed coefficients");
    for frame in 0..=4 {
        assert_eq!(
            transformed.source_ratio_at(frame),
            expected.source_ratio_at(frame)
        );
    }
    assert_eq!(
        transformed
            .for_output_range(0..2)
            .expect("first")
            .followed_by(transformed.for_output_range(2..4).expect("second")),
        Some(transformed)
    );
}

#[kithara::test]
fn exponential_source_spans_reject_unrepresentable_or_invalid_ranges() {
    let rate = NonZeroU32::new(44_100).expect("rate");
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("law");
    assert!(SourceSpan::approaching(0, NonZeroU128::MIN, curve, 0, rate, 0).is_none());
    assert!(SourceSpan::approaching(0, NonZeroU128::MIN, curve, u64::MAX, rate, 1).is_none());
    assert!(SourceSpan::approaching(u128::MAX, NonZeroU128::MIN, curve, 0, rate, 1).is_none());
    let unrepresentable =
        ExponentialApproach::new(f64::MIN_POSITIVE, f64::MIN_POSITIVE, 0.0).expect("finite law");
    assert!(SourceSpan::approaching(0, NonZeroU128::MIN, unrepresentable, 0, rate, 1).is_none());
    let span = SourceSpan::approaching(0, NonZeroU128::MIN, curve, 0, rate, 64).expect("span");
    assert!(
        span.scaled_from(
            (0, NonZeroU128::MIN),
            (0, NonZeroU128::MIN),
            (0, NonZeroU128::MIN)
        )
        .is_none()
    );
    assert!(
        span.scaled_from(
            (1, NonZeroU128::MIN),
            (0, NonZeroU128::MIN),
            (1, NonZeroU128::MIN)
        )
        .is_none()
    );
    assert!(
        span.scaled_from(
            (0, NonZeroU128::MIN),
            (u128::MAX, NonZeroU128::MIN),
            (1, NonZeroU128::MIN)
        )
        .is_none()
    );
}

#[kithara::test]
fn exponential_source_coordinates_preserve_the_nonlinear_interior() {
    let curve = ExponentialApproach::new(1.0, 2.0, 1.0).expect("unit-time law");
    let span = SourceSpan::approaching(
        1,
        NonZeroU128::new(3).expect("third"),
        curve,
        0,
        NonZeroU32::new(44_100).expect("rate"),
        4,
    )
    .expect("span");
    let expected = [
        (1, 3),
        (21919959691, 12884901888),
        (44693454805, 12884901888),
        (34680489113, 6442450944),
        (47362637861, 6442450944),
    ];
    for (frame, (numerator, denominator)) in expected.into_iter().enumerate() {
        assert_eq!(
            span.source_ratio_at(u64::try_from(frame).expect("frame")),
            Some((
                numerator,
                NonZeroU128::new(denominator).expect("denominator")
            ))
        );
    }
}
