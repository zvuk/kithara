use std::num::NonZeroU32;

use kithara_dsp::filter::{Response, StateVariableFilter};
use kithara_test_utils::kithara;

#[kithara::test]
fn allpass_preserves_magnitude_and_reverses_phase_at_its_cutoff() {
    const TOLERANCE: f64 = 1e-5;
    for rate in [48_000_u32, 768_000] {
        for cutoff in [20.0, 1_000.0] {
            let cutoff_period = (f64::from(rate) / cutoff) as usize;
            let mut periods = vec![4, 16, 64, 256, 1_024, rate as usize / 20, cutoff_period];
            periods.sort_unstable();
            periods.dedup();
            for period in periods {
                let warmup = (8 * cutoff_period).max(2 * period);
                let measured = 4 * period;
                let frames = warmup + measured;
                let phase = |frame: usize| std::f64::consts::TAU * frame as f64 / period as f64;
                let mut output: Vec<f32> =
                    (0..frames).map(|frame| phase(frame).sin() as f32).collect();
                let mut filter = StateVariableFilter::<1>::new();
                filter
                    .process(
                        &mut [output.as_mut_slice()],
                        0..frames,
                        &vec![cutoff; frames],
                        NonZeroU32::new(rate).expect("positive rate"),
                        std::f64::consts::FRAC_1_SQRT_2,
                        Response::AllPass,
                    )
                    .expect("valid allpass sweep");
                assert!(output.iter().all(|value| value.is_finite()));
                let (mut real, mut imaginary) = (0.0, 0.0);
                for (frame, value) in output.iter().enumerate().skip(warmup) {
                    let (sine, cosine) = phase(frame).sin_cos();
                    real += f64::from(*value) * sine;
                    imaginary += f64::from(*value) * cosine;
                }
                let scale = 2.0 / measured as f64;
                let (real, imaginary) = (real * scale, imaginary * scale);
                assert!(
                    (real.hypot(imaginary) - 1.0).abs() <= TOLERANCE,
                    "rate{rate} cutoff{cutoff} period{period}: H=({real},{imaginary})"
                );
                if period == cutoff_period {
                    assert!(
                        (real + 1.0).abs() <= TOLERANCE && imaginary.abs() <= TOLERANCE,
                        "the phase at cutoff must be -180 degrees: rate{rate} cutoff{cutoff} H=({real},{imaginary})"
                    );
                }
            }
        }
    }
}
