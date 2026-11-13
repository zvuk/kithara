use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::ExponentialApproach;

#[kithara::test]
fn exponential_approaches_match_independent_unit_time_values() {
    let rising = ExponentialApproach::new(1.0, 2.0, 1.0).expect("rising law");
    assert_eq!(rising.rate_at(0.0), Some(1.0));
    assert_eq!(rising.rate_at(1.0), Some(1.632_120_558_828_557_7));
    assert_eq!(rising.advance(1.0), Some(1.367_879_441_171_442_3));
    let falling = ExponentialApproach::new(2.0, 1.0, 1.0).expect("falling law");
    assert_eq!(falling.rate_at(0.0), Some(2.0));
    assert_eq!(falling.rate_at(1.0), Some(1.367_879_441_171_442_3));
    assert_eq!(falling.advance(1.0), Some(1.632_120_558_828_557_7));
}

#[kithara::test]
fn zero_time_constant_is_an_immediate_target_with_zero_initial_advance() {
    let curve = ExponentialApproach::new(1.0, 3.0, 0.0).expect("instant law");
    assert_eq!(curve.initial(), 1.0);
    assert_eq!(curve.target(), 3.0);
    assert_eq!(curve.time_constant(), 0.0);
    assert_eq!(curve.rate_at(0.0), Some(3.0));
    assert_eq!(curve.advance(0.0), Some(0.0));
    assert_eq!(curve.advance(2.0), Some(6.0));
    assert_eq!(curve.advance_between(7.0, 9.0), Some(6.0));
}

#[kithara::test]
fn a_replacement_before_the_time_constant_preserves_the_reached_rate() {
    let first = ExponentialApproach::new(1.0, 3.0, 4.0).expect("first law");
    let reached = first.rate_at(2.0).expect("reached rate");
    assert_eq!(reached, 1.786_938_680_574_733_2);
    let second = ExponentialApproach::new(reached, 0.75, 4.0).expect("replacement");
    assert_eq!(second.rate_at(0.0), first.rate_at(2.0));
    assert_eq!(second.advance(0.0), Some(0.0));
    assert!(second.rate_at(1.0).expect("approached") < reached);
    assert!(second.rate_at(1.0).expect("approached") > 0.75);
}

#[kithara::test]
fn short_advance_at_a_large_elapsed_time_does_not_subtract_large_areas() {
    let curve = ExponentialApproach::new(1.0, 0.9, 0.005).expect("law");
    assert_eq!(curve.advance_between(1e12, 1e12 + 1.0), Some(0.9));
    assert_ne!(
        curve.advance(1e12 + 1.0).expect("end") - curve.advance(1e12).expect("start"),
        curve.advance_between(1e12, 1e12 + 1.0).expect("interval"),
    );
    assert_eq!(curve.advance_between(1e12, 1e12), Some(0.0));
}

#[kithara::test]
fn rate_scaling_preserves_the_time_axis_and_checks_overflow() {
    let curve = ExponentialApproach::new(2.0, 1.0, 4.0).expect("law");
    let scaled = curve.scaled(0.5).expect("scaled");
    assert_eq!(scaled.initial(), 1.0);
    assert_eq!(scaled.target(), 0.5);
    assert_eq!(scaled.time_constant(), 4.0);
    assert_eq!(
        scaled.advance(4.0),
        curve.advance(4.0).map(|advance| advance * 0.5)
    );
    assert!(curve.scaled(f64::MAX).is_none());
    assert!(curve.scaled(0.0).is_none());
}

#[kithara::test]
fn exponential_approaches_reject_invalid_coefficients_times_and_overflow() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        assert!(ExponentialApproach::new(value, 1.0, 1.0).is_none());
        assert!(ExponentialApproach::new(1.0, value, 1.0).is_none());
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert!(ExponentialApproach::new(1.0, 1.0, value).is_none());
        let curve = ExponentialApproach::new(1.0, 2.0, 1.0).expect("law");
        assert!(curve.rate_at(value).is_none());
        assert!(curve.advance(value).is_none());
        assert!(curve.advance_between(value, 1.0).is_none());
        assert!(curve.advance_between(0.0, value).is_none());
        assert!(curve.advance_over(value, 1.0).is_none());
        assert!(curve.advance_over(0.0, value).is_none());
        assert!(curve.scaled(value).is_none());
    }
    let curve = ExponentialApproach::new(f64::MAX, f64::MAX, 1.0).expect("finite law");
    assert!(curve.advance(2.0).is_none());
    assert!(curve.advance_over(0.0, 2.0).is_none());
    assert!(curve.advance_between(2.0, 1.0).is_none());
}

#[kithara::test]
fn an_integer_duration_survives_absolute_times_above_f64_integer_precision() {
    let curve = ExponentialApproach::new(1.0, 0.9, 220.5).expect("law");
    let start_frame = (1u64 << 53) + 3;
    let end_frame = start_frame + 1;
    let start = start_frame.to_f64().expect("start time");
    let end = end_frame.to_f64().expect("end time");
    assert_eq!(start, end);
    assert_eq!(curve.advance_between(start, end), Some(0.0));
    let elapsed = (end_frame - start_frame)
        .to_f64()
        .expect("integer duration");
    assert_eq!(curve.advance_over(start, elapsed), Some(0.9));
    assert_eq!(curve.advance_over(start, 0.0), Some(0.0));
}
