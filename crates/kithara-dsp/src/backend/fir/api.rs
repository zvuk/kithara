use fearless_simd::{Level, dispatch};

/// Ordered polyphase reconstruction with preprocessed input history.
/// `HISTORY - 1` taps precede each interval's base. The caller supplies the
/// current samples separately from raw future input; no samples are sanitized.
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

    /// Invalidate held-tail intervals at an external block boundary, and prove
    /// whether reconstruction can exceed `threshold`. Complete channel tails
    /// and history participate; a non-finite sample prevents quiet pruning.
    /// Call once per external block, never at internal processing boundaries.
    /// Returns whether reconstruction is required for this block.
    pub fn begin_block(&mut self, channels: &[&mut [f32]], threshold: f32) -> bool {
        dispatch!(Level::new(), simd => self.begin_block_kernel(simd, channels, threshold));
        self.reconstruct
    }

    /// Write sample/adjacent-interval peaks for one channel's common prefix.
    /// `raw` starts at this chunk and includes the remaining external block,
    /// including any unprocessed tail; beyond it, the last raw sample is held.
    /// `current` supplies the preprocessed samples recorded once in history.
    /// Internal chunks retain the shared interval. Invalid channels return zero.
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
            let retained = HISTORY.min(len);
            history.copy_within(retained.., 0);
            for (slot, &sample) in history.iter_mut().rev().zip(current.iter().take(len).rev()) {
                *slot = sample;
            }
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
