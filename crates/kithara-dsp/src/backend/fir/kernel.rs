use fearless_simd::prelude::*;

use super::Fir;
use crate::envelope::PeakHold;

impl<const TAPS: usize, const PHASES: usize, const CHANNELS: usize, const HISTORY: usize>
    Fir<TAPS, PHASES, CHANNELS, HISTORY>
{
    #[inline(always)]
    pub(super) fn begin_block_kernel<S: Simd>(
        &mut self,
        _simd: S,
        channels: &[&mut [f32]],
        threshold: f32,
    ) {
        self.shared_valid.fill(false);
        if channels
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0)
            == 0
        {
            self.reconstruct = false;
            return;
        }
        let maximum = channels
            .iter()
            .flat_map(|channel| channel.iter())
            .chain(self.history.iter().take(channels.len()).flatten())
            .fold(0.0_f32, |maximum, &sample| {
                maximum.max(if sample.is_finite() {
                    sample.abs()
                } else {
                    f32::INFINITY
                })
            });
        self.reconstruct = maximum > threshold
            || self.taps.iter().any(|branch| {
                branch.iter().fold(0.0_f32, |sum, coefficient| {
                    coefficient.abs().mul_add(maximum, sum)
                }) > threshold
            });
    }

    #[inline(always)]
    pub(super) fn process_kernel<S: Simd>(
        &mut self,
        simd: S,
        channel: usize,
        raw: &[f32],
        current: &[f32],
        peaks: &mut [f32],
    ) -> usize {
        let (Some(history), Some(shared), Some(valid)) = (
            self.history.get_mut(channel),
            self.shared_peak.get_mut(channel),
            self.shared_valid.get_mut(channel),
        ) else {
            return 0;
        };
        let len = raw.len().min(current.len()).min(peaks.len());
        let mut future = raw.iter();
        let mut samples = current.iter();
        let mut output = peaks.iter_mut();
        while let (Some(_), Some(&sample), Some(peak)) =
            (future.next(), samples.next(), output.next())
        {
            *peak = sample.abs().max(0.0);
            let future = future.as_slice();
            let past = &history[HISTORY.min(1)..];
            let ahead = Self::interval(simd, &self.taps, past, sample, future, 0);
            let behind = if *valid {
                *shared
            } else {
                Self::interval(
                    simd,
                    &self.taps,
                    history,
                    sample,
                    future,
                    usize::from(HISTORY == 0),
                )
            };
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
    }

    #[inline(always)]
    fn interval<S: Simd>(
        simd: S,
        taps: &[[f32; TAPS]; PHASES],
        past: &[f32],
        current: f32,
        future: &[f32],
        padding: usize,
    ) -> f32 {
        if TAPS == 0 || PHASES == 0 {
            return 0.0;
        }
        let held = future.last().copied().unwrap_or(current);
        if current == 0.0
            && held == 0.0
            && past.iter().all(|&sample| sample == 0.0)
            && future.iter().take(TAPS).all(|&sample| sample == 0.0)
        {
            // Zero samples yield zero or NaN sums; both fold to a positive-zero peak.
            return 0.0;
        }
        let mut peak = 0.0_f32;
        let zero = S::f32s::splat(simd, 0.0);
        for phases in taps.chunks(S::f32s::LEN) {
            let active = S::f32s::from_fn(simd, |lane| f32::from(u8::from(lane < phases.len())))
                .simd_gt(zero);
            let mut sums = zero;
            let samples = std::iter::repeat_n(0.0, padding)
                .chain(past.iter().copied())
                .chain(std::iter::once(current))
                .chain(future.iter().copied())
                .chain(std::iter::repeat(held));
            for (tap, sample) in samples.take(TAPS).enumerate() {
                let coefficients = S::f32s::from_fn(simd, |lane| {
                    phases
                        .get(lane)
                        .and_then(|branch| branch.get(tap))
                        .copied()
                        .unwrap_or(0.0)
                });
                let sample = active.select(S::f32s::splat(simd, sample), zero);
                sums = coefficients.mul_add_precise(sample, sums);
            }
            for &sum in sums.as_slice().iter().take(phases.len()) {
                peak = peak.max(sum.abs());
            }
        }
        peak
    }
}
