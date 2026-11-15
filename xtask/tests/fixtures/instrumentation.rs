#[cfg(not(feature = "facade"))]
use kithara_test_utils::kithara;

#[kithara::flash(false)]
#[kithara::rtsan_forbid_blocking]
fn energy(samples: &[f32]) -> f32 {
    samples.iter().map(|sample| sample * sample).sum()
}

fn main() {
    assert_eq!(energy(&[0.5, -0.5, 0.5, -0.5]), 1.0);
}
