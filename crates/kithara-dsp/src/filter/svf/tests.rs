use fearless_simd::{Level, dispatch};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{Response, StateVariableFilter};
use crate::consts;

struct Scalar<const CHANNELS: usize> {
    state: [[f64; 2]; CHANNELS],
    quiet: usize,
    reset: bool,
}

impl<const CHANNELS: usize> Scalar<CHANNELS> {
    fn frame(
        &mut self,
        planes: &mut [Vec<f32>],
        frame: usize,
        gain: f64,
        damping: f64,
        response: Response,
    ) {
        let mut quiet = true;
        if !(self.reset && planes.iter().all(|plane| plane[frame].to_bits() == 0)) {
            let a1 = (1.0 + gain * (gain + damping)).recip();
            let a2 = gain * a1;
            let a3 = gain * a2;
            for (plane, state) in planes.iter_mut().zip(self.state.iter_mut()) {
                let input = plane[frame];
                let x = f64::from(input);
                let [s1, s2] = *state;
                let v3 = x - s2;
                let v1 = a1 * s1 + a2 * v3;
                let v2 = s2 + a2 * s1 + a3 * v3;
                *state = [2.0 * v1 - s1, 2.0 * v2 - s2];
                let output = match response {
                    Response::LowPass => v2,
                    Response::HighPass => x - damping * v1 - v2,
                }
                .to_f32()
                .unwrap_or(f32::NAN);
                quiet &= input.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK;
                plane[frame] = output;
            }
            self.reset = false;
        }
        self.quiet = if quiet {
            self.quiet.saturating_add(1).min(consts::SECTION_MEMORY)
        } else {
            0
        };
        if self.quiet == consts::SECTION_MEMORY {
            self.state.fill([0.0; 2]);
            self.reset = true;
        }
    }
}

fn ordered_recurrence<const CHANNELS: usize>(response: Response, damping: f64) {
    for level in [Level::fallback(), Level::new()] {
        let mut actual: Vec<Vec<f32>> = (0..CHANNELS)
            .map(|channel| {
                let mut plane: Vec<_> = [0.0, -0.0, 0.75, -0.25, 1e-10, 0.0, 0.0]
                    .into_iter()
                    .cycle()
                    .take(513)
                    .collect();
                if channel == 0 {
                    plane[257] = f32::NAN;
                }
                plane
            })
            .collect();
        let mut expected = actual.clone();
        let mut filter = StateVariableFilter::<CHANNELS>::new();
        let mut scalar = Scalar {
            state: [[0.0; 2]; CHANNELS],
            quiet: 0,
            reset: true,
        };
        dispatch!(level, simd => {
            for (frame, gain) in [0.0, 1e-310, 0.01, 0.2, 1.0, 1e16]
                .into_iter()
                .cycle()
                .take(513)
                .enumerate()
            {
                scalar.frame(&mut expected, frame, gain, damping, response);
                filter.frame(simd, &mut actual, frame, gain, damping, response);
                for (actual, expected) in actual.iter().zip(&expected) {
                    let (actual, expected) = (actual[frame], expected[frame]);
                    if expected.is_nan() {
                        assert!(actual.is_nan(), "{response:?} at {frame}");
                    } else {
                        assert_eq!(actual.to_bits(), expected.to_bits(), "{response:?} at {frame}");
                    }
                }
                for (channel, expected) in scalar.state.iter().enumerate() {
                    for (actual, expected) in filter.state.iter().map(|plane| plane[channel]).zip(expected) {
                        if expected.is_nan() {
                            assert!(actual.is_nan());
                        } else {
                            assert_eq!(actual.to_bits(), expected.to_bits());
                        }
                    }
                }
                assert_eq!(filter.quiet, scalar.quiet);
                assert_eq!(filter.is_reset(), scalar.reset);
            }
        });
    }
}

#[kithara::test]
#[case(Response::LowPass)]
#[case(Response::HighPass)]
fn simd_recurrence_keeps_scalar_order_history_and_quiet_state(#[case] response: Response) {
    for damping in [0.1, 1.0, 10.0, f64::INFINITY] {
        ordered_recurrence::<1>(response, damping);
        ordered_recurrence::<2>(response, damping);
        ordered_recurrence::<3>(response, damping);
        ordered_recurrence::<6>(response, damping);
    }
}
