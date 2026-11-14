#![cfg(not(target_arch = "wasm32"))]

use kithara_dsp::filter::{Crossover, Hertz};
use kithara_test_utils::kithara;

fn hz(value: f64) -> Hertz<f64> {
    Hertz::from_hz(value).expect("positive frequency")
}

#[kithara::test]
fn a_positive_crossover_keeps_its_low_band_when_constructed_or_retuned() {
    let mut missing = 0;
    for rate in [705_600.0, 768_000.0] {
        for retune in [false, true] {
            let mut crossover =
                Crossover::<1>::new(&[hz(20.0)], hz(if retune { 44_100.0 } else { rate }))
                    .expect("positive crossover below Nyquist");
            if retune {
                crossover
                    .update_sample_rate(hz(rate))
                    .expect("admitted retune");
            }
            let (mut low, mut high) = ([0.0_f32; 1], [0.0_f32; 1]);
            assert_eq!(
                crossover.split(
                    &[&[1.0][..]],
                    &mut [[low.as_mut_slice()], [high.as_mut_slice()]]
                ),
                1
            );
            println!("LOW_BAND,{rate},{retune},{:?},{:?}", low, high);
            assert!(low[0].is_finite() && high[0].is_finite());
            missing += usize::from(low[0] == 0.0);
        }
    }
    assert_eq!(
        missing, 0,
        "an admitted positive LR4 low-pass must have a nonzero impulse coefficient"
    );
}
