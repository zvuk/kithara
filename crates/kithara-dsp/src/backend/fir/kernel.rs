use fearless_simd::{f32x8, prelude::*};

use super::Fir;
use crate::envelope::PeakHold;

impl<const TAPS: usize, const PHASES: usize, const CHANNELS: usize, const HISTORY: usize>
    Fir<TAPS, PHASES, CHANNELS, HISTORY>
{
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
        let mut maximum = 0.0_f32;
        for &sample in channels
            .iter()
            .flat_map(|channel| channel.iter())
            .chain(self.history.iter().take(channels.len()).flatten())
        {
            maximum = maximum.max(if sample.is_finite() {
                sample.abs()
            } else {
                f32::INFINITY
            });
            if maximum > threshold {
                self.reconstruct = true;
                return;
            }
        }
        self.reconstruct = simd.vectorize(|| {
            self.taps.chunks(f32x8::<S>::LEN).any(|phases| {
                let mut sums = f32x8::splat(simd, 0.0);
                for tap in 0..TAPS {
                    let coefficients = f32x8::from_fn(simd, |lane| {
                        phases
                            .get(lane)
                            .map_or(0.0, |branch| branch.get(tap).copied().unwrap_or(0.0).abs())
                    });
                    sums = coefficients.mul_add_precise(f32x8::splat(simd, maximum), sums);
                }
                sums.as_slice()
                    .iter()
                    .take(phases.len())
                    .any(|&sum| sum > threshold)
            })
        });
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
                        for phases in self.taps.chunks(f32x8::<S>::LEN) {
                            let mut ahead_sums = f32x8::splat(simd, 0.0);
                            let mut behind_sums = f32x8::splat(simd, 0.0);
                            let mut previous = history.first().copied().unwrap_or(0.0);
                            for tap in 0..TAPS {
                                let input = if tap <= past.len() {
                                    *past.get(tap).unwrap_or(&sample)
                                } else {
                                    future
                                        .get(tap.saturating_sub(past.len()).saturating_sub(1))
                                        .copied()
                                        .unwrap_or(held)
                                };
                                let coefficients = f32x8::from_fn(simd, |lane| {
                                    phases.get(lane).map_or(0.0, |branch| {
                                        branch.get(tap).copied().unwrap_or(0.0)
                                    })
                                });
                                if ahead_active {
                                    let input = f32x8::from_fn(simd, |lane| {
                                        phases.get(lane).map_or(0.0, |_| input)
                                    });
                                    ahead_sums = coefficients.mul_add_precise(input, ahead_sums);
                                }
                                if behind_active {
                                    let input = f32x8::from_fn(simd, |lane| {
                                        phases.get(lane).map_or(0.0, |_| previous)
                                    });
                                    behind_sums = coefficients.mul_add_precise(input, behind_sums);
                                }
                                previous = input;
                            }
                            for &sum in ahead_sums.as_slice().iter().take(phases.len()) {
                                ahead = ahead.max(sum.abs());
                            }
                            if behind_active {
                                for &sum in behind_sums.as_slice().iter().take(phases.len()) {
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
