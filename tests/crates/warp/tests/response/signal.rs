use kithara_test_fixtures::signal::goertzel_magnitude;

use super::cases::{
    CHANNELS, MIN_SIGNAL_RMS, SAMPLE_RATE, TARGET_WINDOW_FRAMES, TONE_DOMINANCE_RATIO, TONES_HZ,
};

pub(crate) fn signal_rms(samples: &[f32]) -> f64 {
    let mut energy = 0.0;
    let mut frames = 0_u32;
    for frame in samples.chunks_exact(usize::from(CHANNELS)) {
        energy = f64::from(frame[0]).mul_add(f64::from(frame[0]), energy);
        frames += 1;
    }
    if frames == 0 {
        return 0.0;
    }
    (energy / f64::from(frames)).sqrt()
}

pub(crate) fn tone_is_dominant(samples: &[f32], target: usize) -> bool {
    let mut channel = [0.0; TARGET_WINDOW_FRAMES];
    let mut frames = 0;
    for (sample, frame) in channel
        .iter_mut()
        .zip(samples.chunks_exact(usize::from(CHANNELS)))
    {
        *sample = frame[0];
        frames += 1;
    }
    let magnitudes = TONES_HZ.map(|tone| goertzel_magnitude(&channel[..frames], tone, SAMPLE_RATE));
    signal_rms(samples) >= MIN_SIGNAL_RMS
        && magnitudes.iter().enumerate().all(|(index, magnitude)| {
            index == target || magnitudes[target] > magnitude * TONE_DOMINANCE_RATIO
        })
}

pub(crate) fn first_target_onset(
    samples: &[f32],
    command_frame: usize,
    target: usize,
) -> Option<usize> {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    (command_frame + TARGET_WINDOW_FRAMES..=frames).find_map(|end| {
        let start = end - TARGET_WINDOW_FRAMES;
        tone_is_dominant(&samples[start * channels..end * channels], target)
            .then_some(start - command_frame)
    })
}

/// Output frames in `[from, to)` that no span covers.
pub(crate) fn unrendered_frames(spans: &[(u64, u64)], from: u64, to: u64) -> u64 {
    let mut ordered = spans.to_vec();
    ordered.sort_unstable();
    let mut cursor = from;
    let mut missing = 0;
    for (start, end) in ordered {
        if cursor >= to || start >= to {
            break;
        }
        if end <= cursor {
            continue;
        }
        missing += start.saturating_sub(cursor);
        cursor = end.min(to).max(cursor);
    }
    missing + to.saturating_sub(cursor)
}
