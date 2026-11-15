use std::ops::{Deref, DerefMut};

use fearless_simd::{Level, dispatch, f32x4, f64x2, prelude::*, u64x2};

use crate::{backend::svf, consts, filter::Response};

struct Section<const CHANNELS: usize> {
    state: [[f64; CHANNELS]; 2],
    quiet: [u64; CHANNELS],
    coefficients: [f64; 3],
}

impl<const CHANNELS: usize> Section<CHANNELS> {
    fn new(gain: f64) -> Self {
        Self {
            state: [[0.0; CHANNELS]; 2],
            quiet: [0; CHANNELS],
            coefficients: svf::coefficients(gain, consts::CROSSOVER_DAMPING),
        }
    }

    #[inline(always)]
    fn run<S: Simd, P: DerefMut<Target = [f32]>>(
        &mut self,
        simd: S,
        planes: &mut [P; CHANNELS],
        frames: usize,
        response: Response,
    ) {
        let [state1, state2] = &mut self.state;
        let (pairs, tail) = planes.as_chunks_mut::<2>();
        let (memory1, tail1) = state1.as_chunks_mut::<2>();
        let (memory2, tail2) = state2.as_chunks_mut::<2>();
        let (quiet, tail_quiet) = self.quiet.as_chunks_mut::<2>();
        for ((([left, right], memory1), memory2), quiet) in
            pairs.iter_mut().zip(memory1).zip(memory2).zip(quiet)
        {
            let frames = prefix(left, frames)
                .iter_mut()
                .zip(prefix(right, frames))
                .map(|(left, right)| [left, right]);
            run_frames(
                simd,
                frames,
                [memory1, memory2],
                quiet,
                self.coefficients,
                response,
            );
        }
        if let ([plane], [memory1], [memory2], [quiet]) = (tail, tail1, tail2, tail_quiet) {
            let frames = prefix(plane, frames).iter_mut().map(|sample| [sample]);
            run_frames(
                simd,
                frames,
                [std::slice::from_mut(memory1), std::slice::from_mut(memory2)],
                std::slice::from_mut(quiet),
                self.coefficients,
                response,
            );
        }
    }

    fn reset(&mut self) {
        self.state.fill([0.0; CHANNELS]);
        self.quiet.fill(0);
    }
}

#[inline(always)]
fn prefix<P: DerefMut<Target = [f32]>>(plane: &mut P, frames: usize) -> &mut [f32] {
    plane
        .get_mut(..frames)
        .expect("frame belongs to the validated common prefix")
}

#[inline(always)]
fn run_frames<'a, S: Simd, const LANES: usize>(
    simd: S,
    frames: impl Iterator<Item = [&'a mut f32; LANES]>,
    [memory1, memory2]: [&mut [f64]; 2],
    quiet: &mut [u64],
    coefficients: [f64; 3],
    response: Response,
) {
    let mut s1 = f64x2::from_fn(simd, |lane| memory1.get(lane).copied().unwrap_or(0.0));
    let mut s2 = f64x2::from_fn(simd, |lane| memory2.get(lane).copied().unwrap_or(0.0));
    let mut count = u64x2::from_fn(simd, |lane| quiet.get(lane).copied().unwrap_or(0));
    let zero = f64x2::splat(simd, 0.0);
    let no_quiet = u64x2::splat(simd, 0);
    let one = u64x2::splat(simd, 1);
    let limit = u64x2::splat(
        simd,
        u64::try_from(consts::SECTION_MEMORY).expect("quiet memory fits in a SIMD lane"),
    );
    let threshold = f32x4::splat(simd, consts::SILENT_PEAK);
    let coefficients = coefficients.map(|value| f64x2::splat(simd, value));
    let damping = f64x2::splat(simd, consts::CROSSOVER_DAMPING);
    for samples in frames {
        let input = f32x4::from_fn(simd, |lane| {
            samples.get(lane).map_or(0.0, |sample| **sample)
        });
        let ([next1, next2], output) = svf::step(
            simd,
            input.widen().0,
            [s1, s2],
            coefficients,
            damping,
            response,
        );
        let output = output.narrow(output);
        let silent = (input.abs().simd_le(threshold) & output.abs().simd_le(threshold))
            .widen()
            .0;
        count = silent.select(count.saturating_add(one).min(limit), no_quiet);
        let retired = count.simd_eq(limit);
        s1 = retired.select(zero, next1);
        s2 = retired.select(zero, next2);
        for (sample, output) in samples.into_iter().zip(output.as_slice()) {
            *sample = *output;
        }
    }
    for (memory, value) in memory1.iter_mut().zip(s1.as_slice()) {
        *memory = *value;
    }
    for (memory, value) in memory2.iter_mut().zip(s2.as_slice()) {
        *memory = *value;
    }
    for (quiet, value) in quiet.iter_mut().zip(count.as_slice()) {
        *quiet = *value;
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
            section.coefficients = svf::coefficients(gain, consts::CROSSOVER_DAMPING);
        }
        for (section, gain) in self.allpass.iter_mut().zip(later) {
            section.coefficients = svf::coefficients(gain, consts::CROSSOVER_DAMPING);
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
fn copy_planes<const CHANNELS: usize, I: Deref<Target = [f32]>, P: DerefMut<Target = [f32]>>(
    input: &[I; CHANNELS],
    output: &mut [P; CHANNELS],
    frames: usize,
) {
    for (input, output) in input.iter().zip(output) {
        prefix(output, frames).copy_from_slice(
            input
                .get(..frames)
                .expect("frame belongs to the validated common prefix"),
        );
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
    let (high, lower) = bands.split_last_mut().expect("validated high band");
    copy_planes(input, high, frames);
    for (split, low) in splits.iter_mut().zip(lower) {
        copy_planes(high, low, frames);
        for section in &mut split.lowpass {
            section.run(simd, low, frames, Response::LowPass);
        }
        for section in &mut split.highpass {
            section.run(simd, high, frames, Response::HighPass);
        }
        for section in &mut split.allpass {
            section.run(simd, low, frames, Response::AllPass);
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
        for retune in [false, true] {
            if retune {
                for splits in [&mut native, &mut fallback] {
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
                        vec![
                            std::array::from_fn(|_| vec![-7.0; frames.saturating_add(1)]);
                            bands.saturating_add(1)
                        ];
                    let mut expected = actual.clone();
                    process(
                        &mut native,
                        &input,
                        actual.get_mut(..bands).expect("required band planes"),
                        frames,
                    );
                    dispatch!(Level::fallback(), simd => process_kernel(simd, &mut fallback, &input, expected.get_mut(..bands).expect("required band planes"), frames));
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
            parity::<3>(bands);
            parity::<6>(bands);
        }
    }
}
