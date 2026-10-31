#![forbid(unsafe_code)]

use kithara_effects::{Ceiling, LimiterConfig, ReleaseMs};
use kithara_test_utils::kithara;

#[kithara::test]
fn ceiling_refuses_non_finite_and_out_of_range_values() {
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
        -0.0,
        0.0,
        f32::MIN_POSITIVE,
        0.001_f32.next_down(),
        1.0_f32.next_up(),
        1.5,
        f32::MAX,
    ] {
        assert!(Ceiling::try_from(value).is_err(), "ceiling {value}");
    }
    for value in [0.001_f32, 0.25, 0.5, 0.98, 1.0] {
        assert_eq!(
            f32::from(Ceiling::try_from(value).unwrap()).to_bits(),
            value.to_bits(),
        );
    }
}

#[kithara::test]
fn release_refuses_non_finite_and_out_of_range_values() {
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
        -5.0,
        -0.0,
        0.0,
        f32::MIN_POSITIVE,
        1.0_f32.next_down(),
        5_000.0_f32.next_up(),
        f32::MAX,
    ] {
        assert!(ReleaseMs::try_from(value).is_err(), "release {value}");
    }
    for value in [1.0_f32, 50.0, 5_000.0] {
        assert_eq!(
            f32::from(ReleaseMs::try_from(value).unwrap()).to_bits(),
            value.to_bits(),
        );
    }
}

#[kithara::test]
fn typed_config_preserves_defaults_and_accepts_range_endpoints() {
    let default = LimiterConfig::default();
    assert_eq!(default.ceiling(), Ceiling::DEFAULT);
    assert_eq!(default.release_ms(), ReleaseMs::DEFAULT);
    assert_eq!(f32::from(default.ceiling()).to_bits(), 0.98_f32.to_bits());
    assert_eq!(
        f32::from(default.release_ms()).to_bits(),
        50.0_f32.to_bits()
    );
    assert_eq!(LimiterConfig::builder().build(), default);
    for ceiling in [Ceiling::MIN, Ceiling::MAX] {
        for release_ms in [ReleaseMs::MIN, ReleaseMs::MAX] {
            let config = LimiterConfig::builder()
                .ceiling(ceiling)
                .release_ms(release_ms)
                .build();
            assert_eq!(config.ceiling(), ceiling);
            assert_eq!(config.release_ms(), release_ms);
        }
    }
}
