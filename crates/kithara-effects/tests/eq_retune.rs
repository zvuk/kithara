#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_effects::{GainDb, eq::EqConfig};
use kithara_test_utils::{bufpool::pools, kithara};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[kithara::test(native)]
fn stereo_eq_preparation_and_commit_preserve_pcm_without_allocation() {
    use kithara_effects::eq::{EqLayout, StereoEq, generate_log_spaced_bands};

    let config = EqConfig::builder(pools()).build();
    let initial = NonZeroU32::new(44_100).unwrap();
    let rate = NonZeroU32::new(48_000).unwrap();
    let layout = |count| {
        Box::new(EqLayout::new(&config, &generate_log_spaced_bands(count), initial).unwrap())
    };
    let make = || {
        let mut eq = StereoEq::with_layout(&config, initial, layout(3)).unwrap();
        eq.set_gain(0, GainDb::from(-12.0));
        assert!(eq.take_layout(layout(3)).unwrap().is_none());
        eq.process(&mut [0.25], &mut [-0.125]);
        assert!(eq.take_layout(layout(10)).unwrap().is_none());
        eq.set_gain(2, GainDb::from(6.0));
        eq
    };
    let mut actual = make();
    let mut reference = make();
    assert_no_alloc(|| actual.prepare_sample_rate(rate)).unwrap();
    let (mut left, mut right) = ([0.25; 13], [-0.125; 13]);
    let (mut expected_left, mut expected_right) = (left, right);
    actual.process(&mut left, &mut right);
    reference.process(&mut expected_left, &mut expected_right);
    assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
    assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    assert_eq!(
        assert_no_alloc(|| actual.apply_prepared_sample_rate()),
        Some(rate)
    );
    assert_no_alloc(|| reference.prepare_sample_rate(rate)).unwrap();
    assert_eq!(
        assert_no_alloc(|| reference.apply_prepared_sample_rate()),
        Some(rate)
    );
    for _ in 0..32 {
        assert_eq!(
            assert_no_alloc(|| actual.apply_prepared_sample_rate()),
            None
        );
        let (mut left, mut right) = ([0.25; 257], [-0.125; 257]);
        let (mut expected_left, mut expected_right) = (left, right);
        actual.process(&mut left, &mut right);
        reference.process(&mut expected_left, &mut expected_right);
        assert_eq!(left.map(f32::to_bits), expected_left.map(f32::to_bits));
        assert_eq!(right.map(f32::to_bits), expected_right.map(f32::to_bits));
    }
    actual.prepare_sample_rate(initial).unwrap();
    let displaced = actual.take_layout(layout(3)).unwrap();
    assert_eq!(actual.apply_prepared_sample_rate(), None);
    drop(displaced);
}
