use std::ops::{Deref, DerefMut};

use fearless_simd::{Level, Simd, dispatch};

use super::svf;
use crate::filter::Response;

const DAMPING: f64 = 1.0 / std::f64::consts::FRAC_1_SQRT_2;

struct Section<const CHANNELS: usize> {
    state: [svf::State<1>; CHANNELS],
    gain: f64,
    response: Response,
}

impl<const CHANNELS: usize> Section<CHANNELS> {
    fn new(gain: f64, response: Response) -> Self {
        Self {
            state: std::array::from_fn(|_| svf::State::new()),
            gain,
            response,
        }
    }

    #[inline(always)]
    fn run<S: Simd>(&mut self, simd: S, mut samples: [f32; CHANNELS]) -> [f32; CHANNELS] {
        svf::independent_frame(
            simd,
            &mut self.state,
            &mut samples,
            self.gain,
            DAMPING,
            self.response,
        );
        samples
    }

    fn reset(&mut self) {
        for state in &mut self.state {
            state.reset();
        }
    }
}

pub(crate) struct Split<const CHANNELS: usize> {
    lowpass: [Section<CHANNELS>; 2],
    highpass: [Section<CHANNELS>; 2],
    allpass: Box<[Section<CHANNELS>]>,
}

impl<const CHANNELS: usize> Split<CHANNELS> {
    pub(crate) fn new(gain: f64, later: impl Iterator<Item = f64>) -> Self {
        Self {
            lowpass: std::array::from_fn(|_| Section::new(gain, Response::LowPass)),
            highpass: std::array::from_fn(|_| Section::new(gain, Response::HighPass)),
            allpass: later
                .map(|gain| Section::new(gain, Response::AllPass))
                .collect(),
        }
    }

    pub(crate) fn reset(&mut self) {
        for section in self
            .lowpass
            .iter_mut()
            .chain(&mut self.highpass)
            .chain(&mut self.allpass)
        {
            section.reset();
        }
    }

    pub(crate) fn retune(&mut self, gain: f64, later: impl Iterator<Item = f64>) {
        for section in self.lowpass.iter_mut().chain(&mut self.highpass) {
            section.gain = gain;
        }
        for (section, gain) in self.allpass.iter_mut().zip(later) {
            section.gain = gain;
        }
        self.reset();
    }
}

pub(crate) fn process<
    const CHANNELS: usize,
    I: Deref<Target = [f32]>,
    P: DerefMut<Target = [f32]>,
>(
    splits: &mut [Split<CHANNELS>],
    input: &[I; CHANNELS],
    bands: &mut [[P; CHANNELS]],
    frames: usize,
) {
    dispatch!(Level::new(), simd => process_kernel(simd, splits, input, bands, frames));
}

#[inline(always)]
fn process_kernel<
    S: Simd,
    const CHANNELS: usize,
    I: Deref<Target = [f32]>,
    P: DerefMut<Target = [f32]>,
>(
    simd: S,
    splits: &mut [Split<CHANNELS>],
    input: &[I; CHANNELS],
    bands: &mut [[P; CHANNELS]],
    frames: usize,
) {
    for frame in 0..frames {
        let mut high = input.each_ref().map(|plane| plane[frame]);
        let (high_band, lower) = bands.split_last_mut().expect("validated high band");
        for (split, band) in splits.iter_mut().zip(lower) {
            let mut low = high;
            for section in &mut split.lowpass {
                low = section.run(simd, low);
            }
            for section in &mut split.highpass {
                high = section.run(simd, high);
            }
            for section in &mut split.allpass {
                low = section.run(simd, low);
            }
            for (plane, value) in band.iter_mut().zip(low) {
                plane[frame] = value;
            }
        }
        for (plane, value) in high_band.iter_mut().zip(high) {
            plane[frame] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn parity<const CHANNELS: usize>(bands: usize) {
        let gains: Vec<_> = (0..bands - 1)
            .map(|index| 0.01 * (index + 1) as f64)
            .collect();
        let make = || {
            gains
                .iter()
                .enumerate()
                .map(|(index, gain)| Split::new(*gain, gains.iter().copied().skip(index + 1)))
                .collect::<Vec<Split<CHANNELS>>>()
        };
        let mut native = make();
        let mut fallback = make();
        for retune in [false, true] {
            if retune {
                for splits in [&mut native, &mut fallback] {
                    for (index, split) in splits.iter_mut().enumerate() {
                        split.retune(
                            gains[index] * 0.5,
                            gains.iter().skip(index + 1).map(|gain| gain * 0.5),
                        );
                    }
                }
            }
            for reset in [false, true] {
                if reset {
                    for split in native.iter_mut().chain(&mut fallback) {
                        split.reset();
                    }
                }
                for frames in [0, 1, 7, 255, 256, 257, 8192] {
                    let input: [Vec<f32>; CHANNELS] = std::array::from_fn(|channel| {
                        if frames == 8192 {
                            return vec![0.0; frames];
                        }
                        [0.0, -0.0, 1.0, -0.5, f32::MIN_POSITIVE, f32::from_bits(1)]
                            .into_iter()
                            .cycle()
                            .skip(channel)
                            .take(frames)
                            .collect()
                    });
                    let mut actual =
                        vec![std::array::from_fn(|_| vec![-7.0; frames + 1]); bands + 1];
                    let mut expected = actual.clone();
                    process(&mut native, &input, &mut actual[..bands], frames);
                    dispatch!(Level::fallback(), simd => process_kernel(simd, &mut fallback, &input, &mut expected[..bands], frames));
                    for (actual, expected) in actual
                        .iter()
                        .flatten()
                        .flatten()
                        .zip(expected.iter().flatten().flatten())
                    {
                        assert_eq!(
                            actual.to_bits(),
                            expected.to_bits(),
                            "channels{CHANNELS} bands{bands} frames{frames}"
                        );
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn native_and_fallback_crossovers_keep_exact_state_across_partitions_and_retunes() {
        for bands in [1, 3, 10] {
            parity::<1>(bands);
            parity::<2>(bands);
        }
    }
}
