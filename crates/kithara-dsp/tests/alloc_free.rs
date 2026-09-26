#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use fearless_simd::Level;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_dsp::Accelerate;
use kithara_dsp::{Backend, Portable};
use kithara_test_utils::kithara;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const FRAMES: usize = 1024;
const SIX: NonZeroUsize = NonZeroUsize::MIN.saturating_add(5);

fn run_every_kernel<B: Backend>(backend: &B) {
    let (left, right) = (vec![0.25_f32; FRAMES], vec![-0.25_f32; FRAMES]);
    let mut pair = vec![0.0_f32; 2 * FRAMES];
    let (mut out_left, mut out_right) = (vec![0.0_f32; FRAMES], vec![0.0_f32; FRAMES]);
    let mut six = vec![0.0_f32; 6 * FRAMES];
    let mut plane = vec![0.0_f32; FRAMES];
    assert_no_alloc(|| {
        backend.interleave_pair(&left, &right, &mut pair);
        backend.deinterleave_pair(&pair, &mut out_left, &mut out_right);
        backend.scatter(&left, &mut six[1..], SIX);
        backend.gather(&six[1..], SIX, &mut plane);
    });
}

#[kithara::test(native, flash(false))]
fn layout_kernels_never_allocate() {
    run_every_kernel(&Portable::default());
    run_every_kernel(&Portable::new(Level::fallback()));
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    run_every_kernel(&Accelerate);
}
