#![forbid(unsafe_code)]

use std::fmt::Debug;

use kithara_effects::{
    FxParamChange, FxParamMapping, FxParamUnit,
    reverb::{Damping, Decay, Reverb, ReverbConfig, Width},
};
use kithara_test_utils::{bufpool::pools, kithara};

fn assert_domain<T: Copy + Debug + PartialEq + Into<f64>>(
    minimum: T,
    maximum: T,
    default: T,
    checked: fn(f64) -> Option<T>,
) {
    let min = minimum.into();
    let max = maximum.into();
    for value in [
        min.next_down(),
        max.next_up(),
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(checked(value).is_none(), "refuse {value}");
    }
    for value in [min, max, default.into(), min + (max - min) * 0.5] {
        assert_eq!(
            checked(value).expect("admitted value").into().to_bits(),
            value.to_bits()
        );
    }
    assert_eq!(checked(default.into()), Some(default));
}

#[kithara::test]
fn reverb_controls_admit_their_bounds_and_refuse_nonfinite_or_out_of_range_values() {
    assert_domain(Decay::MIN, Decay::MAX, Decay::DEFAULT, Decay::checked);
    assert_domain(
        Damping::MIN,
        Damping::MAX,
        Damping::DEFAULT,
        Damping::checked,
    );
    assert_domain(Width::MIN, Width::MAX, Width::DEFAULT, Width::checked);
    assert_eq!(Decay::default(), Decay::DEFAULT);
    assert_eq!(Damping::default(), Damping::DEFAULT);
    assert_eq!(Width::default(), Width::DEFAULT);
    assert!(f64::from(Decay::MIN) > 0.0);
}

#[kithara::test]
fn reverb_native_domains_use_ranged_bounds_and_seconds() {
    let mut expected = None;
    for rate in [8000, 48000, 96000] {
        let mut reverb = Reverb::new(
            std::num::NonZeroU32::new(rate).unwrap(),
            ReverbConfig::default(),
            &pools(),
        )
        .unwrap();
        let descriptors = reverb.parameters().to_vec();
        let bounds = [
            (
                f64::from(Decay::MIN),
                f64::from(Decay::MAX),
                FxParamUnit::Seconds,
                FxParamMapping::Log,
            ),
            (
                f64::from(Damping::MIN),
                f64::from(Damping::MAX),
                FxParamUnit::Ratio,
                FxParamMapping::Linear,
            ),
            (
                f64::from(Width::MIN),
                f64::from(Width::MAX),
                FxParamUnit::Ratio,
                FxParamMapping::Linear,
            ),
        ];
        assert_eq!(descriptors.len(), bounds.len());
        for (descriptor, (min, max, unit, mapping)) in descriptors.iter().zip(bounds) {
            assert_eq!(descriptor.range(), &(min..=max));
            assert_eq!(descriptor.unit(), unit);
            assert_eq!(descriptor.mapping(), Some(mapping));
            assert!(descriptor.contains(min));
            assert!(descriptor.contains(max));
            assert!(!descriptor.contains(f64::NAN));
        }
        let storage = reverb.parameters().as_ptr();
        for (index, value) in [
            f64::from(Decay::MAX),
            f64::from(Damping::MIN),
            f64::from(Width::MIN),
        ]
        .into_iter()
        .enumerate()
        {
            reverb.set(FxParamChange { index, value }).unwrap();
        }
        assert_eq!(storage, reverb.parameters().as_ptr());
        assert_eq!(descriptors, reverb.parameters());
        assert!(reverb.tail_exhausted());
        if let Some(expected) = &expected {
            assert_eq!(expected, &descriptors);
        }
        expected = Some(descriptors);
    }
}
