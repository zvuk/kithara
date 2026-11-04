#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use kithara_effects::{Ceiling, LimiterConfig, LookaheadMs, ReleaseMs};
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
    assert_eq!(default.lookahead_ms(), LookaheadMs::DEFAULT);
    assert_eq!(
        f32::from(default.lookahead_ms()).to_bits(),
        0.0_f32.to_bits()
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

#[kithara::test]
fn lookahead_refuses_non_finite_and_out_of_range_values() {
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
        -f32::MIN_POSITIVE,
        20.0_f32.next_up(),
        f32::MAX,
    ] {
        assert!(LookaheadMs::try_from(value).is_err(), "lookahead {value}");
    }
    for value in [0.0_f32, f32::MIN_POSITIVE, 5.0, 20.0] {
        assert_eq!(
            f32::from(LookaheadMs::try_from(value).unwrap()).to_bits(),
            value.to_bits(),
        );
    }
}

#[kithara::test]
fn latency_is_the_lookahead_rounded_to_the_nearest_frame() {
    for (lookahead_ms, rate, frames) in [
        (0.0_f32, 48_000_u32, 0_usize),
        (0.01, 48_000, 0),
        (0.0125, 48_000, 1),
        (5.0, 48_000, 240),
        (10.0, 44_100, 441),
        (20.0, 48_000, 960),
        (20.0, u32::MAX, 85_899_346),
    ] {
        let config = LimiterConfig::builder()
            .lookahead_ms(LookaheadMs::try_from(lookahead_ms).unwrap())
            .build();
        assert_eq!(
            config.latency_frames(NonZeroU32::new(rate).unwrap()),
            frames,
            "{lookahead_ms} ms at {rate} Hz"
        );
    }
}
