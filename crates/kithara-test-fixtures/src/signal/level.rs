//! Level measurements over rendered PCM: peak, RMS, mean magnitude, silence
//! runs, and the left channel of an interleaved buffer.

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
    let (sum, count) = samples
        .iter()
        .fold((0.0_f32, 0.0_f32), |(sum, count), sample| {
            (sum + sample.abs(), count + 1.0)
        });
    sum / count
}

/// Largest absolute sample value.
#[must_use]
pub fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0_f32, |current, sample| current.max(sample.abs()))
}

/// Root mean square of a sample slice.
#[must_use]
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let (sum_sq, count) = samples
        .iter()
        .fold((0.0_f32, 0.0_f32), |(sum_sq, count), sample| {
            (sum_sq + sample * sample, count + 1.0)
        });
    (sum_sq / count).sqrt()
}

#[cfg(all(test, feature = "native-fixtures", not(target_arch = "wasm32")))]
mod tests {
    use kithara_test_utils::kithara;

    use super::{deinterleave_left, max_silence_run, mean_abs, peak, rms};

    #[kithara::test(native, flash(false))]
    fn a_full_scale_square_has_unit_rms_and_peak() {
        let square = [1.0_f32, -1.0, 1.0, -1.0];
        assert!((rms(&square) - 1.0).abs() < f32::EPSILON);
        assert!((peak(&square) - 1.0).abs() < f32::EPSILON);
        assert!((mean_abs(&square) - 1.0).abs() < f32::EPSILON);
    }

    #[kithara::test(native, flash(false))]
    fn silence_has_zero_rms() {
        assert_eq!(rms(&[0.0; 4]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[kithara::test(native, flash(false))]
    fn the_longest_quiet_stretch_is_counted_inside_the_window() {
        let samples = [0.5_f32, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.5];
        assert_eq!(max_silence_run(&samples, 0, samples.len(), 0.1), 3);
        assert_eq!(max_silence_run(&samples, 0, 4, 0.1), 2);
    }

    #[kithara::test(native, flash(false))]
    fn the_left_channel_is_every_first_sample_of_a_frame() {
        assert_eq!(deinterleave_left(&[1.0, 2.0, 3.0, 4.0], 2), vec![1.0, 3.0]);
    }
}
