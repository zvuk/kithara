#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_effects::{
    LimiterConfig, LookaheadMs, PeakLimiter,
    reverb::{Decay, Reverb, ReverbConfig, Width},
};
use kithara_test_utils::{bufpool::pools, kithara};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[kithara::test(native)]
fn reverb_process_reset_and_tail_retirement_never_allocate() {
    let config = ReverbConfig::builder()
        .decay(Decay::MIN)
        .width(Width::checked(0.37).unwrap())
        .build();
    let mut reverb = Reverb::new(NonZeroU32::new(8000).unwrap(), config, &pools()).unwrap();
    let silence = [0.0_f32; 4096];
    let mut left = [0.0_f32; 4096];
    let mut right = left;
    assert_no_alloc(|| {
        for (index, value) in [
            (0, f64::from(Decay::MAX)),
            (1, f64::from(kithara_effects::reverb::Damping::MAX)),
            (2, f64::from(Width::MIN)),
            (0, f64::from(Decay::MIN)),
        ] {
            reverb
                .set(kithara_effects::FxParamChange { index, value })
                .unwrap();
        }
        reverb
            .process([&[1.0], &[-0.5]], [&mut left[..1], &mut right[..1]])
            .unwrap();
        reverb
            .set(kithara_effects::FxParamChange {
                index: 0,
                value: f64::from(Decay::MAX),
            })
            .unwrap();
        reverb
            .set(kithara_effects::FxParamChange {
                index: 0,
                value: f64::from(Decay::MIN),
            })
            .unwrap();
        reverb
            .process([&silence, &silence], [&mut left, &mut right])
            .unwrap();
        assert!(reverb.tail_exhausted());
        reverb.reset();
        for frames in [0, 1, 17, 513, 4096] {
            reverb
                .process(
                    [&silence[..frames], &silence[..frames]],
                    [&mut left[..frames], &mut right[..frames]],
                )
                .unwrap();
        }
    });
    assert!(
        left.iter()
            .chain(&right)
            .all(|sample| sample.to_bits() == 0)
    );
}

#[kithara::test(native)]
fn limiter_process_retune_and_reset_never_allocate() {
    let mut limiter = PeakLimiter::new(
        NonZeroU32::new(48_000).unwrap(),
        NonZeroU32::new(96_000).unwrap(),
        NonZeroUsize::new(2).unwrap(),
        LimiterConfig::builder()
            .lookahead_ms(LookaheadMs::try_from(5.0).unwrap())
            .build(),
        &pools(),
    )
    .unwrap();
    let mut left = [0.0; 600];
    let mut right = [0.0; 607];
    assert_no_alloc(|| {
        for frames in [600, 1, 0, 7, 255, 256, 257, 1, 0, 600] {
            for amplitude in [0.1, 2.0, 0.98, 0.001, 0.0] {
                left.fill(amplitude);
                right.fill(-amplitude);
                limiter.process_planar(&mut [&mut left[..frames], &mut right[..frames + 7]]);
            }
            if frames == 0 {
                limiter.reset();
            }
        }
        limiter.reset();
        for rate in [96_000, 44_100, 48_000] {
            limiter.retune(NonZeroU32::new(rate).unwrap()).unwrap();
        }
    });
}
