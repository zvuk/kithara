pub use crate::signal_pcm::SAW_PERIOD;

/// Detected direction of a saw-tooth signal in decoded PCM.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SignalDirection {
    /// Values increase each frame.
    Ascending,
    /// Values decrease each frame.
    Descending,
    /// Direction could not be determined.
    Unknown,
}

/// Recover saw-tooth phase from a decoded `f32` sample.
#[must_use]
pub fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

/// Circular distance between two phases modulo [`SAW_PERIOD`].
#[must_use]
pub fn phase_distance(a: usize, b: usize) -> usize {
    let d = a.abs_diff(b);
    d.min(SAW_PERIOD - d)
}

/// Detect a saw-tooth direction from a buffer of interleaved `f32` samples.
#[must_use]
pub fn detect_direction(samples: &[f32], channels: usize) -> SignalDirection {
    let frames = samples.len() / channels;
    if channels == 0 || frames < 2 {
        return SignalDirection::Unknown;
    }

    let check_count = 10.min(frames - 1);
    let mut ascending_votes = 0u32;
    let mut descending_votes = 0u32;

    for frame in 0..check_count {
        let current = phase_from_f32(samples[frame * channels]);
        let next = phase_from_f32(samples[(frame + 1) * channels]);
        let expected_asc = (current + 1) % SAW_PERIOD;
        let expected_desc = (current + SAW_PERIOD - 1) % SAW_PERIOD;

        if next == expected_asc {
            ascending_votes += 1;
        } else if next == expected_desc {
            descending_votes += 1;
        }
    }

    if ascending_votes > descending_votes && ascending_votes > 0 {
        SignalDirection::Ascending
    } else if descending_votes > ascending_votes && descending_votes > 0 {
        SignalDirection::Descending
    } else {
        SignalDirection::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kithara;

    #[kithara::test]
    fn phase_helpers_work() {
        assert_eq!(phase_from_f32(-1.0), 0);
        assert_eq!(phase_distance(0, SAW_PERIOD - 1), 1);
        assert_eq!(
            detect_direction(&[-1.0, -1.0, -0.9999695, -0.9999695], 2),
            SignalDirection::Ascending
        );
    }
}
