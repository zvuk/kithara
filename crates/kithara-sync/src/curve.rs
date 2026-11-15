use std::{
    fmt,
    hash::{BuildHasher, Hash, Hasher},
    num::NonZeroU64,
};

use foldhash::fast::FixedState;
use kithara_platform::sync::Arc;
use kithara_signal::ExponentialApproach;

/// Speed a renderer holds from the output frame a command applies on.
#[derive(Clone, PartialEq)]
pub enum SpeedCurve {
    /// One speed: media seconds consumed per output second.
    Constant(f32),
    /// Analytic speed approach with a time constant in physical output frames.
    Approach(ExponentialApproach),
    /// Ordered analytic segments, each relative to its physical output-frame boundary.
    ApproachSteps(Arc<[(u64, ExponentialApproach)]>),
    /// Linear speed change from the speed at acceptance, then holds `to`.
    Ramp { to: f32, frames: NonZeroU64 },
    /// Speeds at strictly increasing output-frame offsets, then holds the last speed.
    Steps(Arc<[(u64, f32)]>),
}

impl fmt::Debug for SpeedCurve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Constant(speed) => f.debug_tuple("Constant").field(speed).finish(),
            Self::Approach(approach) => f.debug_tuple("Approach").field(approach).finish(),
            Self::ApproachSteps(steps) => {
                let mut hasher = FixedState::default().build_hasher();
                "ApproachSteps".hash(&mut hasher);
                steps.len().hash(&mut hasher);
                for (offset, approach) in steps.iter() {
                    offset.hash(&mut hasher);
                    for value in [
                        approach.initial(),
                        approach.target(),
                        approach.time_constant(),
                    ] {
                        let bits = if value == 0.0 { 0 } else { value.to_bits() };
                        bits.hash(&mut hasher);
                    }
                }
                f.debug_struct("ApproachSteps")
                    .field("count", &steps.len())
                    .field("first", &steps.first())
                    .field("last", &steps.last())
                    .field("fingerprint", &format_args!("{:016x}", hasher.finish()))
                    .finish()
            }
            Self::Ramp { to, frames } => f
                .debug_struct("Ramp")
                .field("to", to)
                .field("frames", frames)
                .finish(),
            Self::Steps(steps) => {
                let mut hasher = FixedState::default().build_hasher();
                "Steps".hash(&mut hasher);
                steps.len().hash(&mut hasher);
                for (offset, speed) in steps.iter() {
                    offset.hash(&mut hasher);
                    let bits = if *speed == 0.0 { 0 } else { speed.to_bits() };
                    bits.hash(&mut hasher);
                }
                f.debug_struct("Steps")
                    .field("count", &steps.len())
                    .field("first", &steps.first())
                    .field("last", &steps.last())
                    .field("fingerprint", &format_args!("{:016x}", hasher.finish()))
                    .finish()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn approach(initial: f64, target: f64, time_constant: f64) -> ExponentialApproach {
        ExponentialApproach::new(initial, target, time_constant).expect("valid fixture approach")
    }

    fn fingerprint(curve: &SpeedCurve) -> String {
        let text = format!("{curve:?}");
        let (_, suffix) = text
            .rsplit_once("fingerprint: ")
            .expect("sequence fingerprint");
        let digest = suffix.strip_suffix(" }").expect("sequence summary end");
        assert_eq!(digest.len(), 16, "{text}");
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{text}"
        );
        digest.to_owned()
    }

    #[kithara::test]
    fn scalar_debug_preserves_variant_names_and_fields() {
        #[derive(Debug)]
        enum Previous {
            Constant(f32),
            Approach(ExponentialApproach),
            Ramp { to: f32, frames: NonZeroU64 },
        }

        let frames = NonZeroU64::new(7).expect("nonzero fixture frames");
        let scalars = [
            0.0,
            -0.0,
            1.25,
            -2.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ]
        .into_iter()
        .flat_map(|speed| {
            [
                Previous::Constant(speed),
                Previous::Ramp { to: speed, frames },
            ]
        })
        .chain([Previous::Approach(approach(1.0, 2.0, 240.0))]);
        for previous in scalars {
            let curve = match previous {
                Previous::Constant(speed) => SpeedCurve::Constant(speed),
                Previous::Approach(law) => SpeedCurve::Approach(law),
                Previous::Ramp { to, frames } => SpeedCurve::Ramp { to, frames },
            };
            assert_eq!(format!("{curve:?}"), format!("{previous:?}"));
            assert_eq!(format!("{curve:#?}"), format!("{previous:#?}"));
        }
    }

    #[kithara::test]
    fn sequence_debug_is_bounded_for_empty_singleton_and_long_curves() {
        for count in [0_u64, 1, 4_096] {
            let steps: Arc<[_]> = (0..count)
                .map(|offset| (offset, 1.25))
                .collect::<Vec<_>>()
                .into();
            let laws: Arc<[_]> = (0..count)
                .map(|offset| (offset, approach(1.0, 2.0, 240.0)))
                .collect::<Vec<_>>()
                .into();
            for (curve, prefix) in [
                (
                    SpeedCurve::Steps(steps.clone()),
                    format!(
                        "Steps {{ count: {count}, first: {:?}, last: {:?}, fingerprint: ",
                        steps.first(),
                        steps.last()
                    ),
                ),
                (
                    SpeedCurve::ApproachSteps(laws.clone()),
                    format!(
                        "ApproachSteps {{ count: {count}, first: {:?}, last: {:?}, fingerprint: ",
                        laws.first(),
                        laws.last()
                    ),
                ),
            ] {
                let text = format!("{curve:?}");
                assert_eq!(text, format!("{prefix}{} }}", fingerprint(&curve)));
                assert!(text.len() <= 512, "{text}");
                assert!(format!("{curve:#?}").len() <= 1_024);
            }
        }
    }

    #[kithara::test]
    fn sequence_tag_and_count_enter_the_fingerprint() {
        let empty_steps = SpeedCurve::Steps(vec![].into());
        let empty_laws = SpeedCurve::ApproachSteps(vec![].into());
        assert_ne!(fingerprint(&empty_steps), fingerprint(&empty_laws));
        for (single, repeated) in [
            (
                SpeedCurve::Steps(vec![(0, 1.0)].into()),
                SpeedCurve::Steps(vec![(0, 1.0); 2].into()),
            ),
            (
                SpeedCurve::ApproachSteps(vec![(0, approach(1.0, 2.0, 3.0))].into()),
                SpeedCurve::ApproachSteps(vec![(0, approach(1.0, 2.0, 3.0)); 2].into()),
            ),
        ] {
            assert_ne!(fingerprint(&single), fingerprint(&repeated));
        }
    }

    #[kithara::test]
    fn every_interior_step_offset_and_speed_enter_the_fingerprint() {
        let steps = vec![(0, 1.0), (10, 1.25), (20, 1.5), (30, 1.75), (40, 2.0)];
        let original = fingerprint(&SpeedCurve::Steps(steps.clone().into()));
        for (index, (offset, speed)) in steps
            .iter()
            .copied()
            .enumerate()
            .take(steps.len() - 1)
            .skip(1)
        {
            for replacement in [
                (offset + 1, speed),
                (offset, f32::from_bits(speed.to_bits() + 1)),
            ] {
                let mut changed = steps.clone();
                changed[index] = replacement;
                assert_eq!(changed.len(), steps.len());
                assert_eq!(changed.first(), steps.first());
                assert_eq!(changed.last(), steps.last());
                assert_ne!(fingerprint(&SpeedCurve::Steps(changed.into())), original);
            }
        }
    }

    #[kithara::test]
    fn every_interior_approach_offset_and_parameter_enter_the_fingerprint() {
        let law = approach(1.0, 2.0, 3.0);
        let steps = vec![(0, law), (10, law), (20, law), (30, law), (40, law)];
        let original = fingerprint(&SpeedCurve::ApproachSteps(steps.clone().into()));
        for (index, (offset, _)) in steps
            .iter()
            .copied()
            .enumerate()
            .take(steps.len() - 1)
            .skip(1)
        {
            for replacement in [
                (offset + 1, law),
                (
                    offset,
                    approach(f64::from_bits(law.initial().to_bits() + 1), 2.0, 3.0),
                ),
                (
                    offset,
                    approach(1.0, f64::from_bits(law.target().to_bits() + 1), 3.0),
                ),
                (
                    offset,
                    approach(1.0, 2.0, f64::from_bits(law.time_constant().to_bits() + 1)),
                ),
            ] {
                let mut changed = steps.clone();
                changed[index] = replacement;
                assert_eq!(changed.len(), steps.len());
                assert_eq!(changed.first(), steps.first());
                assert_eq!(changed.last(), steps.last());
                assert_ne!(
                    fingerprint(&SpeedCurve::ApproachSteps(changed.into())),
                    original
                );
            }
        }
    }

    #[kithara::test]
    fn sequence_fingerprints_include_interior_order() {
        let mut steps = vec![(0, 1.0), (10, 1.25), (20, 1.5), (30, 2.0)];
        let mut laws = vec![
            (0, approach(1.0, 2.0, 3.0)),
            (10, approach(1.25, 2.0, 3.0)),
            (20, approach(1.5, 2.0, 3.0)),
            (30, approach(2.0, 2.0, 3.0)),
        ];
        let original_steps = fingerprint(&SpeedCurve::Steps(steps.clone().into()));
        let original_laws = fingerprint(&SpeedCurve::ApproachSteps(laws.clone().into()));
        steps.swap(1, 2);
        laws.swap(1, 2);
        assert_ne!(
            fingerprint(&SpeedCurve::Steps(steps.into())),
            original_steps
        );
        assert_ne!(
            fingerprint(&SpeedCurve::ApproachSteps(laws.into())),
            original_laws
        );
    }

    #[kithara::test]
    fn equal_signed_zeros_have_equal_fingerprints() {
        let steps = vec![(0, 0.0), (10, 0.0), (20, 0.0)];
        let laws = vec![
            (0, approach(1.0, 2.0, 0.0)),
            (10, approach(1.0, 2.0, 0.0)),
            (20, approach(1.0, 2.0, 0.0)),
        ];
        let positive_steps = SpeedCurve::Steps(steps.clone().into());
        let positive_laws = SpeedCurve::ApproachSteps(laws.clone().into());
        for (index, _) in steps.iter().enumerate() {
            let mut negative_steps = steps.clone();
            negative_steps[index].1 = -0.0;
            let negative_steps = SpeedCurve::Steps(negative_steps.into());
            assert_eq!(positive_steps, negative_steps);
            assert_eq!(fingerprint(&positive_steps), fingerprint(&negative_steps));
            let mut negative_laws = laws.clone();
            negative_laws[index].1 = approach(1.0, 2.0, -0.0);
            let negative_laws = SpeedCurve::ApproachSteps(negative_laws.into());
            assert_eq!(positive_laws, negative_laws);
            assert_eq!(fingerprint(&positive_laws), fingerprint(&negative_laws));
        }
    }

    #[kithara::test]
    fn nonfinite_steps_preserve_raw_data_and_nan_payloads() {
        let mut fingerprints = Vec::new();
        for speed in [
            -2.0,
            f32::from_bits(1),
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::from_bits(0x7fc0_0001),
            f32::from_bits(0x7fc0_0002),
            f32::from_bits(0x7f80_0001),
        ] {
            let steps: Arc<[_]> = vec![(0, speed), (10, speed), (20, speed)].into();
            let before: Vec<_> = steps
                .iter()
                .map(|(offset, value)| (*offset, value.to_bits()))
                .collect();
            let curve = SpeedCurve::Steps(steps.clone());
            let digest = fingerprint(&curve);
            assert!(!fingerprints.contains(&digest));
            fingerprints.push(digest);
            assert!(format!("{curve:#?}").len() <= 1_024);
            let after: Vec<_> = steps
                .iter()
                .map(|(offset, value)| (*offset, value.to_bits()))
                .collect();
            assert_eq!(after, before);
        }
    }
}
