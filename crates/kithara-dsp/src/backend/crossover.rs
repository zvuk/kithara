use std::ops::{Deref, DerefMut};

use fearless_simd::{Level, dispatch, f64x2, prelude::*};
use num_traits::AsPrimitive;

use crate::{
    backend::svf,
    consts,
    filter::{Response, coefficients},
};

struct Section<const CHANNELS: usize> {
    state: [[f64; CHANNELS]; 2],
    quiet: [usize; CHANNELS],
    coefficients: [f64; 3],
}

impl<const CHANNELS: usize> Section<CHANNELS> {
    fn new(gain: f64) -> Self {
        Self {
            state: [[0.0; CHANNELS]; 2],
            quiet: [0; CHANNELS],
            coefficients: coefficients(gain, consts::CROSSOVER_DAMPING),
        }
    }

    #[inline(always)]
    fn run<S: Simd>(
        &mut self,
        simd: S,
        mut samples: [f32; CHANNELS],
        response: Response,
    ) -> [f32; CHANNELS] {
        let [state1, state2] = &mut self.state;
        for (((samples, memory1), memory2), quiet) in samples
            .chunks_mut(2)
            .zip(state1.chunks_mut(2))
            .zip(state2.chunks_mut(2))
            .zip(self.quiet.chunks_mut(2))
        {
            let x = f64x2::from_fn(simd, |lane| {
                samples.get(lane).copied().map_or(0.0, f64::from)
            });
            let ([next1, next2], output) = svf::step(
                simd,
                x,
                [load_pair(simd, memory1), load_pair(simd, memory2)],
                self.coefficients,
                consts::CROSSOVER_DAMPING,
                response,
            );
            let memory = memory1.iter_mut().zip(memory2);
            let next = next1.as_slice().iter().zip(next2.as_slice());
            let output = samples.iter_mut().zip(output.as_slice());
            for (((sample, output), (memory1, memory2)), (quiet, (next1, next2))) in
                output.zip(memory).zip(quiet.iter_mut().zip(next))
            {
                let input = *sample;
                *sample = output.as_();
                let silent =
                    input.abs() <= consts::SILENT_PEAK && sample.abs() <= consts::SILENT_PEAK;
                *quiet = if silent {
                    quiet.saturating_add(1).min(consts::SECTION_MEMORY)
                } else {
                    0
                };
                if *quiet == consts::SECTION_MEMORY {
                    *memory1 = 0.0;
                    *memory2 = 0.0;
                } else {
                    *memory1 = *next1;
                    *memory2 = *next2;
                }
            }
        }
        samples
    }

    #[inline(always)]
    fn run_scalar(&mut self, channel: usize, input: f32, response: Response) -> f32 {
        let [memory1, memory2] = &mut self.state;
        let memory1 = memory1.get_mut(channel).expect("validated scalar channel");
        let memory2 = memory2.get_mut(channel).expect("validated scalar channel");
        let ([next1, next2], output) = svf::step_scalar(
            f64::from(input),
            [*memory1, *memory2],
            self.coefficients,
            consts::CROSSOVER_DAMPING,
            response,
        );
        *memory1 = next1;
        *memory2 = next2;
        let output: f32 = output.as_();
        let quiet = self
            .quiet
            .get_mut(channel)
            .expect("validated scalar channel");
        let limit = consts::SECTION_MEMORY;
        *quiet = if input.abs() <= consts::SILENT_PEAK && output.abs() <= consts::SILENT_PEAK {
            quiet.saturating_add(1).min(limit)
        } else {
            0
        };
        if *quiet == limit {
            *memory1 = 0.0;
            *memory2 = 0.0;
        }
        output
    }

    fn reset(&mut self) {
        self.state.fill([0.0; CHANNELS]);
        self.quiet.fill(0);
    }
}

#[inline(always)]
fn load_pair<S: Simd>(simd: S, memory: &[f64]) -> f64x2<S> {
    match memory {
        [one] => f64x2::from_slice(simd, &[*one, 0.0]),
        pair => f64x2::from_slice(simd, pair),
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
            lowpass: std::array::from_fn(|_| Section::new(gain)),
            highpass: std::array::from_fn(|_| Section::new(gain)),
            allpass: later.map(Section::new).collect(),
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
            section.coefficients = coefficients(gain, consts::CROSSOVER_DAMPING);
        }
        for (section, gain) in self.allpass.iter_mut().zip(later) {
            section.coefficients = coefficients(gain, consts::CROSSOVER_DAMPING);
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

pub(crate) fn process_scalar<
    const CHANNELS: usize,
    I: Deref<Target = [f32]>,
    P: DerefMut<Target = [f32]>,
>(
    splits: &mut [Split<CHANNELS>],
    input: &[I; CHANNELS],
    bands: &mut [[P; CHANNELS]],
    frames: usize,
) {
    let (high_band, lower) = bands.split_last_mut().expect("validated high band");
    for (channel, (input, high_plane)) in input.iter().zip(high_band.iter_mut()).enumerate() {
        let input = input.get(..frames).expect("validated scalar prefix");
        let high_plane = high_plane
            .get_mut(..frames)
            .expect("validated scalar high frames");
        for (frame, (&sample, high_out)) in input.iter().zip(high_plane).enumerate() {
            let mut high = sample;
            for (split, band) in splits.iter_mut().zip(lower.iter_mut()) {
                let mut low = high;
                for section in &mut split.lowpass {
                    low = section.run_scalar(channel, low, Response::LowPass);
                }
                for section in &mut split.highpass {
                    high = section.run_scalar(channel, high, Response::HighPass);
                }
                for section in &mut split.allpass {
                    low = section.run_scalar(channel, low, Response::AllPass);
                }
                *band
                    .get_mut(channel)
                    .and_then(|plane| plane.get_mut(frame))
                    .expect("validated scalar band frame") = low;
            }
            *high_out = high;
        }
    }
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
        let mut high = input.each_ref().map(|plane| {
            *plane
                .get(frame)
                .expect("frame belongs to the validated common prefix")
        });
        let (high_band, lower) = bands.split_last_mut().expect("validated high band");
        for (split, band) in splits.iter_mut().zip(lower) {
            let mut low = high;
            for section in &mut split.lowpass {
                low = section.run(simd, low, Response::LowPass);
            }
            for section in &mut split.highpass {
                high = section.run(simd, high, Response::HighPass);
            }
            for section in &mut split.allpass {
                low = section.run(simd, low, Response::AllPass);
            }
            for (plane, value) in band.iter_mut().zip(low) {
                *plane
                    .get_mut(frame)
                    .expect("frame belongs to the validated common prefix") = value;
            }
        }
        for (plane, value) in high_band.iter_mut().zip(high) {
            *plane
                .get_mut(frame)
                .expect("frame belongs to the validated common prefix") = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::*;

    fn parity<const CHANNELS: usize>(bands: usize) {
        let gains: Vec<_> = (0..bands.checked_sub(1).expect("one high band"))
            .map(|index| {
                0.01 * index
                    .saturating_add(1)
                    .to_f64()
                    .expect("bounded section index")
            })
            .collect();
        let make = || {
            gains
                .iter()
                .enumerate()
                .map(|(index, gain)| {
                    Split::new(*gain, gains.iter().copied().skip(index.saturating_add(1)))
                })
                .collect::<Vec<Split<CHANNELS>>>()
        };
        let mut native = make();
        let mut fallback = make();
        let mut scalar = make();
        for retune in [false, true] {
            if retune {
                for splits in [&mut native, &mut fallback, &mut scalar] {
                    for (index, split) in splits.iter_mut().enumerate() {
                        split.retune(
                            *gains.get(index).expect("prepared split gain") * 0.5,
                            gains
                                .iter()
                                .skip(index.saturating_add(1))
                                .map(|gain| gain * 0.5),
                        );
                    }
                }
            }
            for reset in [false, true] {
                if reset {
                    for split in native.iter_mut().chain(&mut fallback).chain(&mut scalar) {
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
                        vec![
                            std::array::from_fn(|_| vec![-7.0; frames.saturating_add(1)]);
                            bands.saturating_add(1)
                        ];
                    let mut expected = actual.clone();
                    let mut scalar_output = actual.clone();
                    process(
                        &mut native,
                        &input,
                        actual.get_mut(..bands).expect("required band planes"),
                        frames,
                    );
                    dispatch!(Level::fallback(), simd => process_kernel(simd, &mut fallback, &input, expected.get_mut(..bands).expect("required band planes"), frames));
                    process_scalar(
                        &mut scalar,
                        &input,
                        scalar_output
                            .get_mut(..bands)
                            .expect("required band planes"),
                        frames,
                    );
                    for (actual, expected) in scalar_output
                        .iter()
                        .flatten()
                        .flatten()
                        .zip(expected.iter().flatten().flatten())
                    {
                        assert_eq!(
                            actual.to_bits(),
                            expected.to_bits(),
                            "scalar channels{CHANNELS} bands{bands} frames{frames}"
                        );
                    }
                    for candidate in [&native, &scalar] {
                        for (actual, expected) in candidate.iter().zip(&fallback) {
                            let actual = actual
                                .lowpass
                                .iter()
                                .chain(&actual.highpass)
                                .chain(&actual.allpass);
                            let expected = expected
                                .lowpass
                                .iter()
                                .chain(&expected.highpass)
                                .chain(&expected.allpass);
                            for (actual, expected) in actual.zip(expected) {
                                assert_eq!(actual.quiet, expected.quiet);
                                for (actual, expected) in actual
                                    .state
                                    .iter()
                                    .flatten()
                                    .zip(expected.state.iter().flatten())
                                {
                                    assert_eq!(
                                        actual.to_bits(),
                                        expected.to_bits(),
                                        "carried channels{CHANNELS} bands{bands} frames{frames}"
                                    );
                                }
                            }
                        }
                    }
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

    #[kithara::test(timeout(Medium))]
    fn scalar_simd_and_fallback_crossovers_keep_exact_state_across_partitions_and_retunes() {
        for bands in [1, 3, 10] {
            parity::<1>(bands);
            parity::<2>(bands);
            parity::<3>(bands);
            parity::<6>(bands);
        }
    }
}
