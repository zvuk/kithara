use fearless_simd::Simd;

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
        _simd: S,
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
        if !self.reconstruct {
            while let (Some(_), Some(&sample), Some(peak)) =
                (future.next(), samples.next(), output.next())
            {
                *peak = sample.abs().max(0.0);
            }
            if len > 0 {
                let retained = HISTORY.min(len);
                history.copy_within(retained.., 0);
                let (_, retained_history) = history.split_at_mut(HISTORY.saturating_sub(retained));
                let (processed, _) = current.split_at(len);
                let (_, retained_current) = processed.split_at(len.saturating_sub(retained));
                retained_history.copy_from_slice(retained_current);
                *valid = true;
            }
            return len;
        }
        while let (Some(_), Some(&sample), Some(peak)) =
            (future.next(), samples.next(), output.next())
        {
            *peak = sample.abs().max(0.0);
            let future = future.as_slice();
            let ahead = Self::interval(&self.taps, &history[HISTORY.min(1)..], sample, future, 0);
            let behind = if *valid {
                *shared
            } else {
                Self::interval(
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
    fn interval(
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
        for branch in taps {
            let mut sum = 0.0_f32;
            let mut zeros = 0..padding;
            let mut history = past.iter();
            let mut middle = Some(current);
            let mut following = future.iter();
            for &coefficient in branch {
                let sample = if zeros.next().is_some() {
                    0.0
                } else if let Some(&sample) = history.next() {
                    sample
                } else if let Some(sample) = middle.take() {
                    sample
                } else {
                    following.next().copied().unwrap_or(held)
                };
                sum = coefficient.mul_add(sample, sum);
            }
            peak = peak.max(sum.abs());
        }
        peak
    }
}
