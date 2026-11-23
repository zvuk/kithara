use kithara_platform::sync::Arc;
use kithara_signal::ExponentialApproach;
use kithara_sync::SpeedCurve;
use num_traits::ToPrimitive;

use super::super::settings::check_speed;
use crate::PlayError;

pub(super) fn check_curve(curve: &mut SpeedCurve) -> Result<f32, PlayError> {
    match curve {
        SpeedCurve::Constant(speed) | SpeedCurve::Ramp { to: speed, .. } => {
            *speed = check_speed(*speed)?;
            Ok(*speed)
        }
        SpeedCurve::Steps(steps) => {
            if steps.is_empty() || steps.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
                return Err(PlayError::Internal(
                    "speed steps must be nonempty and ordered".into(),
                ));
            }
            for (_, speed) in Arc::make_mut(steps) {
                *speed = check_speed(*speed)?;
            }
            Ok(steps[steps.len() - 1].1)
        }
        SpeedCurve::Approach(approach) => check_approach(*approach),
        SpeedCurve::ApproachSteps(steps) => {
            if steps.first().is_none_or(|(frame, _)| *frame != 0)
                || steps.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            {
                return Err(PlayError::Internal(
                    "approach steps must start at zero and increase".into(),
                ));
            }
            let mut final_speed = 0.0;
            for (_, approach) in steps.iter() {
                final_speed = check_approach(*approach)?;
            }
            Ok(final_speed)
        }
    }
}

fn check_approach(approach: ExponentialApproach) -> Result<f32, PlayError> {
    for rate in [approach.initial(), approach.target()] {
        if !(f64::from(kithara_sync::MIN_SPEED)..=4.0).contains(&rate) {
            return Err(PlayError::InvalidParameter {
                name: "speed".into(),
                value: rate.to_f32().ok_or(PlayError::Untimed)?,
            });
        }
    }
    check_speed(approach.target().to_f32().ok_or(PlayError::Untimed)?)
}

pub(super) fn curve_speed(curve: &SpeedCurve, origin: f64, frame: u64) -> Result<f64, PlayError> {
    match curve {
        SpeedCurve::Constant(speed) => Ok(f64::from(*speed)),
        SpeedCurve::Ramp { to, frames } => (origin
            + (f64::from(*to) - origin)
                * (frame.min(frames.get()).to_f64().ok_or(PlayError::Untimed)?
                    / frames.get().to_f64().ok_or(PlayError::Untimed)?))
        .to_f32()
        .map(f64::from)
        .ok_or(PlayError::Untimed),
        SpeedCurve::Steps(steps) => steps[..steps.partition_point(|(at, _)| *at <= frame)]
            .last()
            .map(|(_, speed)| f64::from(*speed))
            .or_else(|| origin.to_f32().map(f64::from))
            .ok_or(PlayError::Untimed),
        SpeedCurve::Approach(approach) => approach
            .rate_at(frame.to_f64().ok_or(PlayError::Untimed)?)
            .ok_or(PlayError::Untimed),
        SpeedCurve::ApproachSteps(steps) => {
            let &(start, approach) = steps[..steps.partition_point(|(at, _)| *at <= frame)]
                .last()
                .ok_or(PlayError::Untimed)?;
            approach
                .rate_at((frame - start).to_f64().ok_or(PlayError::Untimed)?)
                .ok_or(PlayError::Untimed)
        }
    }
}

pub(super) fn curve_area(
    curve: &SpeedCurve,
    origin: f64,
    start: u64,
    end: u64,
) -> Result<f64, PlayError> {
    match curve {
        SpeedCurve::Constant(speed) => {
            Ok((end - start).to_f64().ok_or(PlayError::Untimed)? * f64::from(*speed))
        }
        SpeedCurve::Ramp { to, frames } => {
            let duration = frames.get().to_f64().ok_or(PlayError::Untimed)?;
            let area = |frame: u64| -> Result<f64, PlayError> {
                let ramp = frame.min(frames.get()).to_f64().ok_or(PlayError::Untimed)?;
                Ok(origin * ramp
                    + (f64::from(*to) - origin) * ramp * ramp / (2.0 * duration)
                    + frame
                        .saturating_sub(frames.get())
                        .to_f64()
                        .ok_or(PlayError::Untimed)?
                        * f64::from(*to))
            };
            Ok(area(end)? - area(start)?)
        }
        SpeedCurve::Steps(steps) => steps_area(steps, origin, start, end),
        SpeedCurve::Approach(approach) => approach
            .advance_over(
                start.to_f64().ok_or(PlayError::Untimed)?,
                end.checked_sub(start)
                    .and_then(|frames| frames.to_f64())
                    .ok_or(PlayError::Untimed)?,
            )
            .ok_or(PlayError::Untimed),
        SpeedCurve::ApproachSteps(steps) => approaches_area(steps, start, end),
    }
}

fn steps_area(steps: &[(u64, f32)], origin: f64, start: u64, end: u64) -> Result<f64, PlayError> {
    let mut area = 0.0;
    let mut cursor = start;
    let mut speed = origin;
    for &(frame, next) in steps {
        if frame > end {
            break;
        }
        if frame > cursor {
            area += (frame - cursor).to_f64().ok_or(PlayError::Untimed)? * speed;
            cursor = frame;
        }
        speed = f64::from(next);
    }
    Ok(area + (end - cursor).to_f64().ok_or(PlayError::Untimed)? * speed)
}

fn approaches_area(
    steps: &[(u64, ExponentialApproach)],
    start: u64,
    end: u64,
) -> Result<f64, PlayError> {
    let mut area = 0.0;
    let mut cursor = start;
    for (index, &(at, approach)) in steps.iter().enumerate() {
        let boundary = steps.get(index + 1).map_or(end, |(next, _)| end.min(*next));
        if boundary <= cursor {
            continue;
        }
        let elapsed = cursor.checked_sub(at).ok_or(PlayError::Untimed)?;
        let frames = boundary.checked_sub(cursor).ok_or(PlayError::Untimed)?;
        area += approach
            .advance_over(
                elapsed.to_f64().ok_or(PlayError::Untimed)?,
                frames.to_f64().ok_or(PlayError::Untimed)?,
            )
            .ok_or(PlayError::Untimed)?;
        cursor = boundary;
        if cursor == end {
            break;
        }
    }
    (cursor == end && area.is_finite())
        .then_some(area)
        .ok_or(PlayError::Untimed)
}

/// The accepted speed law restated from one output frame on, in output frames
/// after it, with every segment's parameters kept as accepted.
#[derive(Default)]
pub(super) struct RestatedLaw {
    steps: Vec<(u64, ExponentialApproach)>,
    constant: Option<f32>,
}

impl RestatedLaw {
    pub(super) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Replaces the law from `offset` on with `curve`, entered `elapsed` frames
    /// after its own start from the speed `origin`: a later command supersedes
    /// every accumulated step at or after its own boundary.
    pub(super) fn restate(
        &mut self,
        curve: &SpeedCurve,
        origin: f64,
        elapsed: u64,
        offset: u64,
    ) -> Result<(), PlayError> {
        self.steps
            .truncate(self.steps.partition_point(|(at, _)| *at < offset));
        let first = self.steps.is_empty();
        match curve {
            SpeedCurve::Constant(speed) => {
                self.constant = first.then_some(*speed);
                self.steps.push((offset, level(f64::from(*speed))?));
            }
            SpeedCurve::Approach(approach) => {
                self.constant = None;
                self.steps.push((offset, resumed(*approach, elapsed)?));
            }
            SpeedCurve::ApproachSteps(steps) => {
                self.constant = None;
                let entered = steps
                    .partition_point(|(at, _)| *at <= elapsed)
                    .checked_sub(1)
                    .ok_or(PlayError::Untimed)?;
                let (began, approach) = steps[entered];
                self.steps
                    .push((offset, resumed(approach, elapsed - began)?));
                for &(at, approach) in &steps[entered + 1..] {
                    self.steps.push((offset + (at - elapsed), approach));
                }
            }
            SpeedCurve::Steps(steps) => {
                self.constant = None;
                self.steps
                    .push((offset, level(curve_speed(curve, origin, elapsed)?)?));
                for &(at, speed) in steps.iter().filter(|(at, _)| *at > elapsed) {
                    self.steps
                        .push((offset + (at - elapsed), level(f64::from(speed))?));
                }
            }
            SpeedCurve::Ramp { .. } => {
                return Err(PlayError::Internal(
                    "a linear speed ramp cannot continue on another source".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<SpeedCurve, PlayError> {
        match (self.steps.as_slice(), self.constant) {
            ([], _) => Err(PlayError::Untimed),
            ([_], Some(speed)) => Ok(SpeedCurve::Constant(speed)),
            ([(_, approach)], None) => Ok(SpeedCurve::Approach(*approach)),
            _ => Ok(SpeedCurve::ApproachSteps(self.steps.into())),
        }
    }
}

fn level(speed: f64) -> Result<ExponentialApproach, PlayError> {
    ExponentialApproach::new(speed, speed, 0.0)
        .ok_or_else(|| PlayError::Internal("speed is not a valid approach level".into()))
}

fn resumed(approach: ExponentialApproach, elapsed: u64) -> Result<ExponentialApproach, PlayError> {
    approach
        .resumed(elapsed)
        .ok_or_else(|| PlayError::Internal("approach course overflows its phase".into()))
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_signal::ExponentialApproach;
    use kithara_sync::SpeedCurve;
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{RestatedLaw, check_curve, curve_area, curve_speed};
    use crate::PlayError;

    fn curve_end(
        curve: &SpeedCurve,
        origin: f64,
        start: u64,
        remaining: f64,
        limit: Option<u64>,
    ) -> Result<Option<u64>, PlayError> {
        use std::num::{NonZeroU32, NonZeroU128};

        use kithara_warp::SourceTrajectory;

        if !remaining.is_finite() || remaining < 0.0 {
            return Err(PlayError::Untimed);
        }
        if start == u64::MAX || limit.is_some_and(|end| end <= start) {
            return Ok(None);
        }
        let rate = NonZeroU32::MIN;
        let mut trajectory = SourceTrajectory::new(origin.to_f32().ok_or(PlayError::Untimed)?);
        trajectory
            .replace(curve.clone())
            .map_err(|_| PlayError::Untimed)?;
        trajectory
            .advance_by(0, rate, start)
            .map_err(|_| PlayError::Untimed)?;
        trajectory.reset();
        let bits = remaining.to_bits();
        let exponent = i32::try_from((bits >> 52) & 2047).map_err(|_| PlayError::Untimed)? - 1075;
        let mantissa = u128::from((bits & 0x000f_ffff_ffff_ffff) | 0x0010_0000_0000_0000);
        let (numerator, denominator) = if remaining == 0.0 {
            (0, 1)
        } else if exponent >= 0 {
            (
                mantissa
                    .checked_shl(exponent.unsigned_abs())
                    .ok_or(PlayError::Untimed)?,
                1,
            )
        } else {
            (
                mantissa,
                1u128
                    .checked_shl(exponent.unsigned_abs())
                    .ok_or(PlayError::Untimed)?,
            )
        };
        let shift = numerator.trailing_zeros().min(denominator.trailing_zeros());
        let numerator = numerator >> shift;
        let denominator = NonZeroU128::new(denominator >> shift).ok_or(PlayError::Untimed)?;
        trajectory
            .first_at_or_after(
                0,
                rate,
                (numerator, denominator),
                limit.map(|end| end - start),
            )
            .map_err(|_| PlayError::Untimed)?
            .map(|offset| start.checked_add(offset).ok_or(PlayError::Untimed))
            .transpose()
    }

    #[kithara::test]
    #[case::rising(1.0, 2.0, 1_068_338_765)]
    #[case::falling(2.0, 1.0, 1_070_756_275)]
    fn an_approach_uses_its_owned_f64_initial_rate(
        #[case] initial: f64,
        #[case] target: f64,
        #[case] expected_bits: u32,
    ) {
        let approach = ExponentialApproach::new(initial, target, 220.5).expect("valid approach");
        let curve = SpeedCurve::Approach(approach);
        assert_eq!(
            curve_speed(&curve, 0.125, 97)
                .expect("observed speed")
                .to_f32()
                .expect("published speed")
                .to_bits(),
            expected_bits
        );
        assert_eq!(
            curve_speed(&curve, 0.125, 97).expect("analytic projection"),
            approach.rate_at(97.0).expect("owned analytic rate")
        );
    }

    #[kithara::test]
    fn opposite_approaches_conserve_their_combined_area() {
        let rising = SpeedCurve::Approach(
            ExponentialApproach::new(1.0, 2.0, 220.5).expect("rising approach"),
        );
        let falling = SpeedCurve::Approach(
            ExponentialApproach::new(2.0, 1.0, 220.5).expect("falling approach"),
        );
        for end in [1, 32, 97, 220, 441, 1_024] {
            assert_eq!(
                curve_area(&rising, 1.0, 0, end).expect("rising area")
                    + curve_area(&falling, 2.0, 0, end).expect("falling area"),
                3.0 * end.to_f64().expect("frame count")
            );
        }
    }

    #[kithara::test]
    #[case::rising(1.0, 2.0, 0, 858)]
    #[case::falling(2.0, 1.0, 0, 1_281)]
    #[case::rising_after_elapsed(1.0, 2.0, 97, 917)]
    #[case::falling_after_elapsed(2.0, 1.0, 97, 1_456)]
    fn an_approach_ends_on_the_first_frame_that_reaches_its_area(
        #[case] initial: f64,
        #[case] target: f64,
        #[case] start: u64,
        #[case] expected: u64,
    ) {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(initial, target, 220.5).expect("valid approach"),
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1_500.0, None).expect("unbounded EOF"),
            Some(expected)
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1_500.0, Some(expected - 1)).expect("before EOF"),
            None
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1_500.0, Some(expected)).expect("bounded EOF"),
            Some(expected)
        );
        assert!(curve_area(&curve, 0.125, start, expected - 1).expect("prior area") < 1_500.0);
        assert!(curve_area(&curve, 0.125, start, expected).expect("terminal area") >= 1_500.0);
    }

    #[kithara::test]
    #[case::rising(1.0, 2.0, 858)]
    #[case::falling(2.0, 1.0, 1_281)]
    fn an_approach_keeps_its_eof_across_quantum_partitions(
        #[case] initial: f64,
        #[case] target: f64,
        #[case] expected: u64,
    ) {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(initial, target, 220.5).expect("valid approach"),
        );
        for quantum in [1, 32, 128, 1_024] {
            let mut cursor = 0;
            let mut consumed = 0.0;
            let end = loop {
                let boundary = cursor + quantum;
                if let Some(end) =
                    curve_end(&curve, 0.125, cursor, 1_500.0 - consumed, Some(boundary))
                        .expect("partition EOF")
                {
                    break end;
                }
                consumed += curve_area(&curve, 0.125, cursor, boundary).expect("partition area");
                cursor = boundary;
                assert!(cursor < expected);
            };
            assert_eq!(end, expected);
        }
    }

    #[kithara::test]
    fn replacement_before_the_time_constant_keeps_the_reached_rate_and_area() {
        let first = ExponentialApproach::new(1.0, 2.0, 220.5).expect("initial approach");
        let first_curve = SpeedCurve::Approach(first);
        let reached = first.rate_at(97.0).expect("reached rate");
        let replacement = SpeedCurve::Approach(
            ExponentialApproach::new(reached, 0.9, 220.5).expect("replacement approach"),
        );
        let carried = curve_area(&first_curve, 1.0, 0, 97).expect("carried area");
        assert_eq!(
            curve_end(&replacement, 0.125, 0, 400.0 - carried, None).expect("replacement EOF"),
            Some(242)
        );
        assert!(carried + curve_area(&replacement, 0.125, 0, 241).expect("prior area") < 400.0);
        assert!(carried + curve_area(&replacement, 0.125, 0, 242).expect("terminal area") >= 400.0);
    }

    #[kithara::test]
    fn observed_f32_speed_does_not_round_the_integrated_area_or_eof() {
        let frames = 1_u64 << 26;
        let rate = 1.0 + 2.0_f64.powi(-26);
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(rate, rate, 220.5).expect("constant approach"),
        );
        let area = frames.to_f64().expect("frame count") + 1.0;
        assert_eq!(
            curve_speed(&curve, 0.125, frames).expect("projection"),
            rate
        );
        assert_eq!(rate.to_f32().expect("published target"), 1.0);
        assert_eq!(
            curve_area(&curve, 0.125, 0, frames).expect("f64 area"),
            area
        );
        assert_eq!(
            curve_end(&curve, 0.125, 0, area, None).expect("f64 EOF"),
            Some(frames)
        );
    }

    #[kithara::test]
    fn a_late_interval_keeps_its_area_without_subtracting_absolute_positions() {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(0.75, 1.75, 220.5).expect("valid approach"),
        );
        let start = 1_u64 << 40;
        assert_eq!(
            curve_area(&curve, 0.125, start, start + 1).expect("late area"),
            1.75
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1.75, Some(start + 1)).expect("late EOF"),
            Some(start + 1)
        );
    }

    #[kithara::test]
    fn an_instant_approach_reaches_the_target_and_keeps_an_exact_eof_boundary() {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(0.75, 1.5, 0.0).expect("instant approach"),
        );
        assert_eq!(curve_speed(&curve, 0.125, 0).expect("instant speed"), 1.5);
        assert_eq!(
            curve_area(&curve, 0.125, 0, 64).expect("instant area"),
            96.0
        );
        assert_eq!(
            curve_end(&curve, 0.125, 0, 96.0, Some(64)).expect("exact EOF"),
            Some(64)
        );
    }

    #[kithara::test]
    fn an_approach_rejects_unrepresentable_area_and_unbounded_remaining() {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(f64::MAX, f64::MAX, 0.0).expect("finite rates"),
        );
        assert!(matches!(
            curve_area(&curve, 1.0, 0, 2),
            Err(PlayError::Untimed)
        ));
        for remaining in [f64::NAN, f64::INFINITY] {
            assert!(matches!(
                curve_end(&curve, 1.0, 0, remaining, None),
                Err(PlayError::Untimed)
            ));
        }
        assert_eq!(
            curve_end(&curve, 1.0, u64::MAX, 1.0, None).expect("empty output range"),
            None
        );
    }

    #[kithara::test]
    fn a_one_frame_interval_above_f64_integer_precision_keeps_its_area_and_eof() {
        let curve = SpeedCurve::Approach(
            ExponentialApproach::new(0.75, 1.75, 220.5).expect("valid approach"),
        );
        let start = (1_u64 << 53) + 3;
        assert_eq!(
            curve_area(&curve, 0.125, start, start + 1).expect("one-frame area"),
            1.75
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1.75, Some(start + 1)).expect("one-frame EOF"),
            Some(start + 1)
        );
    }

    #[kithara::test]
    #[case::steps(false)]
    #[case::approaches(true)]
    fn ordered_curves_keep_their_speed_on_both_sides_of_every_boundary(#[case] analytic: bool) {
        let origin = 0.7_f64;
        let mut curve = if analytic {
            SpeedCurve::ApproachSteps(
                (0..2_048_u64)
                    .map(|index| {
                        let initial = if index % 2 == 0 { 0.75 } else { 1.25 };
                        (
                            index * 4,
                            ExponentialApproach::new(initial, 1.0, 220.5).expect("valid law"),
                        )
                    })
                    .collect(),
            )
        } else {
            SpeedCurve::Steps(
                (0..2_048_u64)
                    .map(|index| (index * 4 + 3, if index % 2 == 0 { 0.75 } else { 1.25 }))
                    .collect(),
            )
        };
        check_curve(&mut curve).expect("accepted ordered curve");
        for frame in (0..8_196).chain([u64::MAX]) {
            let expected = match &curve {
                SpeedCurve::Steps(steps) => {
                    steps.iter().rev().find(|(at, _)| *at <= frame).map_or_else(
                        || f64::from(origin.to_f32().expect("finite origin")),
                        |(_, speed)| f64::from(*speed),
                    )
                }
                SpeedCurve::ApproachSteps(steps) => {
                    let &(at, approach) = steps
                        .iter()
                        .rev()
                        .find(|(at, _)| *at <= frame)
                        .expect("curve begins at zero");
                    approach
                        .rate_at((frame - at).to_f64().expect("elapsed frame"))
                        .expect("finite rate")
                }
                _ => unreachable!("constructed ordered curve"),
            };
            assert_eq!(
                curve_speed(&curve, origin, frame)
                    .expect("ordered projection")
                    .to_bits(),
                expected.to_bits(),
                "frame {frame}"
            );
        }
    }

    #[kithara::test]
    fn approach_steps_change_law_on_their_exact_boundary_and_hold_the_last_law() {
        let first = ExponentialApproach::new(0.75, 1.0, 0.0).expect("first law");
        let second = ExponentialApproach::new(0.75, 2.0, 0.0).expect("second law");
        let curve = SpeedCurve::ApproachSteps(Arc::from([(0, first), (64, second)]));
        assert_eq!(curve_speed(&curve, 0.125, 63).expect("first speed"), 1.0);
        assert_eq!(curve_speed(&curve, 0.125, 64).expect("boundary speed"), 2.0);
        assert_eq!(curve_speed(&curve, 0.125, 1_024).expect("tail speed"), 2.0);
        assert_eq!(curve_area(&curve, 0.125, 0, 96).expect("whole area"), 128.0);
        assert_eq!(
            curve_area(&curve, 0.125, 63, 65).expect("boundary area"),
            3.0
        );
        assert_eq!(curve_area(&curve, 0.125, 65, 97).expect("tail area"), 64.0);
        assert_eq!(
            curve_end(&curve, 0.125, 0, 128.0, Some(95)).expect("before EOF"),
            None
        );
        assert_eq!(
            curve_end(&curve, 0.125, 0, 128.0, None).expect("whole EOF"),
            Some(96)
        );
        assert_eq!(
            curve_end(&curve, 0.125, 63, 3.0, Some(65)).expect("crossing EOF"),
            Some(65)
        );
    }

    #[kithara::test]
    fn approach_steps_keep_the_original_nonlinear_area_and_eof_after_replacement() {
        let first = ExponentialApproach::new(1.0, 2.0, 220.5).expect("first law");
        let second =
            ExponentialApproach::new(first.rate_at(97.0).expect("reached rate"), 0.9, 220.5)
                .expect("second law");
        let curve = SpeedCurve::ApproachSteps(Arc::from([(0, first), (97, second)]));
        assert_eq!(
            curve_speed(&curve, 0.125, 96)
                .expect("pre-boundary speed")
                .to_f32()
                .expect("published speed")
                .to_bits(),
            1_068_314_206
        );
        assert_eq!(
            curve_speed(&curve, 0.125, 97)
                .expect("boundary speed")
                .to_f32()
                .expect("published speed")
                .to_bits(),
            1_068_338_765
        );
        assert_eq!(
            curve_speed(&curve, 0.125, 98)
                .expect("local-law speed")
                .to_f32()
                .expect("published speed")
                .to_bits(),
            1_068_321_460
        );
        for (frame, expected) in [
            (96, first.rate_at(96.0)),
            (97, second.rate_at(0.0)),
            (98, second.rate_at(1.0)),
        ] {
            assert_eq!(
                curve_speed(&curve, 0.125, frame).expect("precise projected speed"),
                expected.expect("owned rate")
            );
        }
        assert!(curve_area(&curve, 0.125, 0, 338).expect("prior area") < 400.0);
        assert!(curve_area(&curve, 0.125, 0, 339).expect("terminal area") >= 400.0);
        assert_eq!(
            curve_end(&curve, 0.125, 0, 400.0, None).expect("composite EOF"),
            Some(339)
        );
        for quantum in [1, 32, 128] {
            let mut cursor = 0;
            let mut consumed = 0.0;
            let end = loop {
                let boundary = cursor + quantum;
                if let Some(end) =
                    curve_end(&curve, 0.125, cursor, 400.0 - consumed, Some(boundary))
                        .expect("partition EOF")
                {
                    break end;
                }
                consumed += curve_area(&curve, 0.125, cursor, boundary).expect("partition area");
                cursor = boundary;
                assert!(cursor < 339);
            };
            assert_eq!(end, 339);
        }
    }

    #[kithara::test]
    fn a_composite_tail_keeps_one_frame_above_f64_integer_precision() {
        let first = ExponentialApproach::new(1.0, 1.0, 0.0).expect("first law");
        let second = ExponentialApproach::new(0.75, 1.75, 220.5).expect("second law");
        let curve = SpeedCurve::ApproachSteps(Arc::from([(0, first), (97, second)]));
        let start = (1_u64 << 53) + 101;
        assert_eq!(
            curve_area(&curve, 0.125, start, start + 1).expect("one-frame tail area"),
            1.75
        );
        assert_eq!(
            curve_end(&curve, 0.125, start, 1.75, Some(start + 1)).expect("one-frame tail EOF"),
            Some(start + 1)
        );
    }

    #[kithara::test]
    #[case::past_the_accepted_tail(400)]
    #[case::inside_the_accepted_tail(100)]
    fn a_restated_law_continues_every_accepted_segment_until_a_later_command(#[case] command: u64) {
        let approach = |initial, target| {
            ExponentialApproach::new(initial, target, 240.0).expect("approach course")
        };
        let accepted = SpeedCurve::ApproachSteps(Arc::from([
            (0, approach(1.0, 1.04)),
            (100, approach(1.03, 0.98)),
            (300, approach(0.99, 1.02)),
        ]));
        let mut law = RestatedLaw::default();
        law.restate(&accepted, 1.0, 150, 0).expect("entered law");
        law.restate(&SpeedCurve::Constant(1.25), 1.02, 0, command)
            .expect("speed command ahead");
        let mut restated = law.finish().expect("restated law");
        check_curve(&mut restated).expect("an ordered law");
        for frame in [0, 1, 99, 100, 101, 149, 150, 151, 399, 400] {
            let expected = if frame < command {
                curve_speed(&accepted, 1.0, 150 + frame).expect("accepted rate")
            } else {
                1.25
            };
            assert_eq!(
                curve_speed(&restated, 1.0, frame).expect("restated rate"),
                expected,
                "frame {frame}"
            );
        }
    }

    #[kithara::test]
    fn a_restated_law_keeps_a_held_speed_and_refuses_a_linear_ramp() {
        let mut held = RestatedLaw::default();
        held.restate(&SpeedCurve::Constant(1.25), 1.0, 7, 0)
            .expect("held speed");
        assert!(matches!(
            held.finish(),
            Ok(SpeedCurve::Constant(speed)) if speed == 1.25
        ));

        let mut ramp = RestatedLaw::default();
        assert!(
            ramp.restate(
                &SpeedCurve::Ramp {
                    to: 1.5,
                    frames: std::num::NonZeroU64::new(64).expect("ramp frames"),
                },
                1.0,
                3,
                0,
            )
            .is_err()
        );
    }
}
