use fearless_simd::{f32x8, prelude::*};

use super::Fir;
use crate::{
    backend::simd::{blocks, update, vector},
    envelope::PeakHold,
};

impl<const TAPS: usize, const PHASES: usize, const CHANNELS: usize, const HISTORY: usize>
    Fir<TAPS, PHASES, CHANNELS, HISTORY>
{
    /// A `NaN` fails `simd_lt`, so every non-finite sample bounds at infinity.
    #[inline(always)]
    pub(super) fn begin_block_kernel<S: Simd>(
        &mut self,
        simd: S,
        channels: &[&mut [f32]],
        threshold: f32,
    ) {
        self.shared_valid.fill(false);
        if channels.is_empty() || channels.iter().any(|channel| channel.is_empty()) {
            self.reconstruct = false;
            return;
        }
        let infinity = S::f32s::splat(simd, f32::INFINITY);
        let mut bound = S::f32s::splat(simd, 0.0);
        for samples in channels.iter().map(|channel| &**channel).chain(
            self.history
                .iter()
                .take(channels.len())
                .map(<[f32; HISTORY]>::as_slice),
        ) {
            blocks::<S>(samples.len(), |start, end| {
                let magnitude = vector(simd, samples.get(start..end).unwrap_or_default()).abs();
                bound = bound.max(magnitude.simd_lt(infinity).select(magnitude, infinity));
            });
        }
        let maximum = bound.reduce_max();
        self.reconstruct = maximum > threshold
            || (0..PHASES).any(|phase| {
                self.taps.iter().fold(0.0_f32, |sum, tap| {
                    tap.get(phase)
                        .copied()
                        .unwrap_or_default()
                        .abs()
                        .mul_add(maximum, sum)
                }) > threshold
            });
    }

    /// Sample magnitudes of a block whose reconstruction stays under the threshold.
    #[inline(always)]
    pub(super) fn magnitude_kernel<S: Simd>(
        &mut self,
        simd: S,
        channel: usize,
        raw: &[f32],
        current: &[f32],
        peaks: &mut [f32],
    ) -> usize {
        let (Some(history), Some(valid)) = (
            self.history.get_mut(channel),
            self.shared_valid.get_mut(channel),
        ) else {
            return 0;
        };
        let len = raw.len().min(current.len()).min(peaks.len());
        let zero = S::f32s::splat(simd, 0.0);
        update(
            simd,
            peaks.get_mut(..len).unwrap_or_default(),
            [current],
            |_, [sample]| {
                let magnitude = sample.abs();
                magnitude.simd_ge(zero).select(magnitude, zero)
            },
        );
        for &sample in current.iter().take(len).skip(len.saturating_sub(HISTORY)) {
            history.copy_within(HISTORY.min(1).., 0);
            if let Some(newest) = history.last_mut() {
                *newest = sample;
            }
        }
        if len > 0 {
            *valid = true;
        }
        len
    }

    #[inline(never)]
    pub(super) fn process_kernel<S: Simd>(
        &mut self,
        simd: S,
        channel: usize,
        raw: &[f32],
        current: &[f32],
        peaks: &mut [f32],
    ) -> usize {
        simd.vectorize(
            #[inline(always)]
            || {
                let (Some(history), Some(shared), Some(valid)) = (
                    self.history.get_mut(channel),
                    self.shared_peak.get_mut(channel),
                    self.shared_valid.get_mut(channel),
                ) else {
                    return 0;
                };
                let len = raw.len().min(current.len()).min(peaks.len());
                let mut future = raw.iter();
                for (peak, &sample) in peaks.iter_mut().take(raw.len()).zip(current) {
                    let _ = future.next();
                    *peak = sample.abs().max(0.0);
                    let future = future.as_slice();
                    let held = future.last().copied().unwrap_or(sample);
                    let past = &history[HISTORY.min(1)..];
                    let zero = sample == 0.0
                        && held == 0.0
                        && future.iter().take(TAPS).all(|&sample| sample == 0.0);
                    let ahead_active =
                        TAPS.min(PHASES) > 0 && !(zero && past.iter().all(|&sample| sample == 0.0));
                    let behind_active = !*valid
                        && TAPS.min(PHASES) > 0
                        && !(zero && history.iter().all(|&sample| sample == 0.0));
                    let mut ahead = 0.0_f32;
                    let mut behind = if *valid { *shared } else { 0.0 };
                    if ahead_active || behind_active {
                        for first_phase in (0..PHASES).step_by(f32x8::<S>::LEN) {
                            let phase_count =
                                PHASES.saturating_sub(first_phase).min(f32x8::<S>::LEN);
                            let zero = f32x8::splat(simd, 0.0);
                            let active =
                                f32x8::from_fn(
                                    simd,
                                    |lane| {
                                        if lane < phase_count { 1.0 } else { 0.0 }
                                    },
                                )
                                .simd_gt(zero);
                            let mut ahead_sums = zero;
                            let mut behind_sums = zero;
                            let mut previous = history.first().copied().unwrap_or(0.0);
                            for (tap, row) in self.taps.iter().enumerate() {
                                let input = if tap <= past.len() {
                                    *past.get(tap).unwrap_or(&sample)
                                } else {
                                    future
                                        .get(tap.saturating_sub(past.len()).saturating_sub(1))
                                        .copied()
                                        .unwrap_or(held)
                                };
                                let mut coefficients = zero;
                                coefficients
                                    .as_mut_slice()
                                    .get_mut(..phase_count)
                                    .unwrap_or_default()
                                    .copy_from_slice(
                                        row.get(
                                            first_phase..first_phase.saturating_add(phase_count),
                                        )
                                        .unwrap_or_default(),
                                    );
                                if ahead_active {
                                    let input = active.select(f32x8::splat(simd, input), zero);
                                    ahead_sums = coefficients.mul_add_precise(input, ahead_sums);
                                }
                                if behind_active {
                                    let input = active.select(f32x8::splat(simd, previous), zero);
                                    behind_sums = coefficients.mul_add_precise(input, behind_sums);
                                }
                                previous = input;
                            }
                            for &sum in ahead_sums.as_slice().iter().take(phase_count) {
                                ahead = ahead.max(sum.abs());
                            }
                            if behind_active {
                                for &sum in behind_sums.as_slice().iter().take(phase_count) {
                                    behind = behind.max(sum.abs());
                                }
                            }
                        }
                    }
                    *shared = ahead;
                    *peak = peak.max(PeakHold::peak(&[ahead, behind]));
                    if !history.is_empty() {
                        history.copy_within(1.., 0);
                        if let Some(newest) = history.last_mut() {
                            *newest = sample;
                        }
                    }
                    *valid = true;
                }
                len
            },
        )
    }
}
