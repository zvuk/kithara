use kithara::play::PlayerEvent;

/// Measurements collected from one offline render window.
#[non_exhaustive]
pub struct WindowStats {
    /// Silent blocks in the window.
    pub silent_blocks: u32,
    /// Total blocks in the window.
    pub total_blocks: u32,
    /// First sample of the window in the output buffer.
    pub window_start_sample: usize,
}

impl WindowStats {
    #[must_use]
    pub const fn new(silent_blocks: u32, total_blocks: u32, window_start_sample: usize) -> Self {
        Self {
            silent_blocks,
            total_blocks,
            window_start_sample,
        }
    }
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TimedPlayerEvent {
    pub frame_end: usize,
    pub event: PlayerEvent,
}

impl TimedPlayerEvent {
    #[must_use]
    pub const fn new(frame_end: usize, event: PlayerEvent) -> Self {
        Self { frame_end, event }
    }
}

/// Copy the left channel from interleaved PCM samples.
#[must_use]
pub fn deinterleave_left(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame[0])
        .collect()
}

/// Longest run below `threshold` within the bounded sample window.
#[must_use]
pub fn max_silence_run(samples: &[f32], start: usize, end: usize, threshold: f32) -> usize {
    let end = end.min(samples.len());
    if end <= start {
        return 0;
    }
    let mut max_run = 0;
    let mut current = 0;
    for sample in &samples[start..end] {
        if sample.abs() < threshold {
            current += 1;
            max_run = max_run.max(current);
        } else {
            current = 0;
        }
    }
    max_run
}

/// Mean absolute sample value.
#[must_use]
pub fn mean_abs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.iter().map(|sample| sample.abs()).sum::<f32>() / samples.len() as f32
}

/// Largest absolute sample value.
#[must_use]
pub fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0_f32, |current, sample| current.max(sample.abs()))
}

/// Root mean square of an interleaved sample slice.
#[must_use]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample count precision is adequate for test windows"
    )]
    let count = samples.len() as f32;
    let sum_sq: f32 = samples.iter().map(|sample| sample * sample).sum();
    (sum_sq / count).sqrt()
}
