use fearless_simd::{Level, dispatch};

/// Ordered polyphase reconstruction from separate preprocessed current/history and raw future.
/// `HISTORY - 1` taps precede each interval's base; no samples are sanitized.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Fir<const TAPS: usize, const PHASES: usize, const CHANNELS: usize, const HISTORY: usize>
{
    pub(super) taps: [[f32; TAPS]; PHASES],
    pub(super) history: [[f32; HISTORY]; CHANNELS],
    pub(super) shared_peak: [f32; CHANNELS],
    pub(super) shared_valid: [bool; CHANNELS],
    pub(super) reconstruct: bool,
}

impl<const TAPS: usize, const PHASES: usize, const CHANNELS: usize, const HISTORY: usize>
    Fir<TAPS, PHASES, CHANNELS, HISTORY>
{
    /// Store phase-major coefficients and start every channel at zero history.
    #[must_use]
    pub fn new(taps: [[f32; TAPS]; PHASES]) -> Self {
        Self {
            taps,
            history: [[0.0; HISTORY]; CHANNELS],
            shared_peak: [0.0; CHANNELS],
            shared_valid: [false; CHANNELS],
            reconstruct: false,
        }
    }

    /// Return whether reconstruction can exceed `threshold`, invalidating held-tail intervals.
    /// Complete channel tails/history and non-finite samples participate in the bound.
    /// Call once per external block, never at internal processing boundaries.
    pub fn begin_block(&mut self, channels: &[&mut [f32]], threshold: f32) -> bool {
        dispatch!(Level::new(), simd => self.begin_block_kernel(simd, channels, threshold));
        self.reconstruct
    }

    /// Write common-prefix sample/adjacent-interval peaks; invalid channels return zero.
    /// `raw` starts at this chunk, includes the external tail, then holds its last sample.
    /// Preprocessed `current` enters history once; internal chunks keep the shared interval.
    #[inline]
    pub fn process(
        &mut self,
        channel: usize,
        raw: &[f32],
        current: &[f32],
        peaks: &mut [f32],
    ) -> usize {
        if self.reconstruct {
            return dispatch!(Level::new(), simd => self.process_kernel(simd, channel, raw, current, peaks));
        }
        let (Some(history), Some(valid)) = (
            self.history.get_mut(channel),
            self.shared_valid.get_mut(channel),
        ) else {
            return 0;
        };
        let len = raw.len().min(current.len()).min(peaks.len());
        for (peak, &sample) in peaks.iter_mut().zip(current).take(raw.len()) {
            *peak = sample.abs().max(0.0);
        }
        if len > 0 {
            history.copy_within(HISTORY.min(len).., 0);
            let (_, target) = history.split_at_mut(HISTORY.saturating_sub(len));
            let (source, _) = current.split_at(len);
            target.copy_from_slice(source.split_at(len.saturating_sub(HISTORY)).1);
            *valid = true;
        }
        len
    }

    /// Forget all input and interval state, preserving the coefficients.
    pub fn reset(&mut self) {
        self.history.fill([0.0; HISTORY]);
        self.shared_peak.fill(0.0);
        self.shared_valid.fill(false);
        self.reconstruct = false;
    }
}
