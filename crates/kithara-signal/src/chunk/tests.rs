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
