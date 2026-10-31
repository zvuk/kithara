#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::{NonZeroU32, NonZeroUsize};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use kithara_effects::{LimiterConfig, PeakLimiter};
use kithara_test_utils::kithara;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[kithara::test(native, flash(false))]
fn limiter_process_and_reset_never_allocate() {
    let mut limiter = PeakLimiter::new(
        NonZeroU32::new(48_000).unwrap(),
        NonZeroUsize::new(2).unwrap(),
        LimiterConfig::default(),
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
    });
}
