use cochlea_features::{Audio, ProbeOpts, SegmentOpts, probe, segment_timeline};
use serde::Serialize;

const WINDOW_MS: f64 = 5.0;

/// Cochlea measurements used by final-PCM acceptance tests and manifests.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CochleaReport {
    /// Integrated program loudness in LUFS, when defined.
    pub integrated_lufs: Option<f64>,
    /// Maximum momentary loudness in LUFS, when defined.
    pub momentary_max_lufs: Option<f64>,
    /// Sample peak in dBFS, when defined.
    pub sample_peak_dbfs: Option<f64>,
    /// True peak in dBTP, when defined.
    pub true_peak_dbtp: Option<f64>,
    /// Number of threshold-silent analysis windows.
    pub silent_segments: usize,
    /// Detected onset timestamps in milliseconds.
    pub onset_times_ms: Vec<f64>,
    /// Number of samples at or beyond full scale.
    pub clipped_samples: usize,
    /// Whether the measured true peak exceeds 0 dBTP.
    pub true_peak_over_0dbtp: bool,
    /// Leading threshold-silence duration in milliseconds.
    pub leading_silence_ms: f64,
    /// Trailing threshold-silence duration in milliseconds.
    pub trailing_silence_ms: f64,
}

impl CochleaReport {
    /// Measure final interleaved PCM with Cochlea.
    #[must_use]
    pub fn measure(samples: &[f32], channels: u16, sample_rate: u32) -> Self {
        let audio = Audio {
            samples: samples.to_vec(),
            channels,
            sample_rate,
        };
        let report = probe(&audio, &ProbeOpts::default());
        let silent_segments =
            segment_timeline(&audio, &SegmentOpts::default().with_window_ms(WINDOW_MS))
                .segments
                .iter()
                .filter(|segment| segment.silent)
                .count();

        Self {
            integrated_lufs: report.loudness.integrated_lufs,
            momentary_max_lufs: report.loudness.momentary_max_lufs,
            sample_peak_dbfs: report.loudness.sample_peak_dbfs,
            true_peak_dbtp: report.loudness.true_peak_dbtp,
            silent_segments,
            onset_times_ms: report.onsets.times_ms,
            clipped_samples: report.clipping.clipped_samples,
            true_peak_over_0dbtp: report.clipping.true_peak_over_0dbtp,
            leading_silence_ms: report.silence.leading_ms,
            trailing_silence_ms: report.silence.trailing_ms,
        }
    }

    /// Return the number of detected onsets.
    #[must_use]
    pub fn onset_count(&self) -> usize {
        self.onset_times_ms.len()
    }
}

/// Compare candidate continuity against a time-aligned control report.
#[must_use]
pub fn continuity_failures(
    label: &str,
    candidate: &CochleaReport,
    control: &CochleaReport,
) -> Vec<String> {
    let mut failures = Vec::new();
    if candidate.silent_segments > control.silent_segments {
        failures.push(format!(
            "{label}: extra silent segments: candidate={}, control={}",
            candidate.silent_segments, control.silent_segments,
        ));
    }
    if candidate.onset_count() != control.onset_count() {
        failures.push(format!(
            "{label}: onset count changed: candidate={}, control={}, candidate_times_ms={:?}, control_times_ms={:?}",
            candidate.onset_count(),
            control.onset_count(),
            candidate.onset_times_ms,
            control.onset_times_ms,
        ));
    }
    if candidate.clipped_samples > control.clipped_samples {
        failures.push(format!(
            "{label}: extra clipped samples: candidate={}, control={}",
            candidate.clipped_samples, control.clipped_samples,
        ));
    }
    if candidate.true_peak_over_0dbtp && !control.true_peak_over_0dbtp {
        failures.push(format!("{label}: candidate-only true peak over 0 dBTP"));
    }
    if candidate.leading_silence_ms > control.leading_silence_ms + WINDOW_MS {
        failures.push(format!(
            "{label}: extra leading silence: candidate={:.3}ms, control={:.3}ms",
            candidate.leading_silence_ms, control.leading_silence_ms,
        ));
    }
    if candidate.trailing_silence_ms > control.trailing_silence_ms + WINDOW_MS {
        failures.push(format!(
            "{label}: extra trailing silence: candidate={:.3}ms, control={:.3}ms",
            candidate.trailing_silence_ms, control.trailing_silence_ms,
        ));
    }
    failures
}

/// Prove that the shared comparator rejects an injected dropout and clipped frame.
///
/// Panics if the control is invalid or either mutation is not detected.
pub fn assert_oracle_load_bearing(
    control: &[f32],
    channels: u16,
    sample_rate: u32,
    missing_frames: usize,
) {
    let channel_count = usize::from(channels);
    assert!(channel_count > 0, "Cochlea oracle needs a channel");
    let control_report = CochleaReport::measure(control, channels, sample_rate);
    assert_eq!(
        control_report.clipped_samples, 0,
        "Cochlea control must not already be clipped"
    );

    let middle_frame = control.len() / channel_count / 2;
    let gap_start = middle_frame.saturating_sub(missing_frames / 2);
    let gap_end = gap_start.saturating_add(missing_frames);
    assert!(
        gap_end.saturating_mul(channel_count) <= control.len(),
        "Cochlea control is too short for the injected dropout"
    );
    let mut gapped = control.to_vec();
    gapped[gap_start * channel_count..gap_end * channel_count].fill(0.0);
    let gap_report = CochleaReport::measure(&gapped, channels, sample_rate);
    assert!(
        continuity_failures("injected dropout", &gap_report, &control_report)
            .iter()
            .any(|failure| failure.contains("silent segments")),
        "Cochlea comparator accepted a {missing_frames}-frame dropout: control={control_report:?}, gapped={gap_report:?}"
    );

    let mut clicked = control.to_vec();
    let click_start = middle_frame.saturating_add(17) * channel_count;
    clicked[click_start..click_start + channel_count].fill(1.0);
    let click_report = CochleaReport::measure(&clicked, channels, sample_rate);
    assert_eq!(
        click_report.clipped_samples,
        control_report.clipped_samples + channel_count,
        "Cochlea oracle did not count one injected clipped frame"
    );
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn comparator_rejects_one_missing_quantum_and_one_clipped_frame() {
        let sample_rate = 48_000;
        let channels = 2;
        let frames = sample_rate as usize * 2;
        let mut control = Vec::with_capacity(frames * usize::from(channels));
        for frame in 0..frames {
            let phase = std::f32::consts::TAU * 440.0 * frame as f32 / sample_rate as f32;
            let sample = phase.sin() * 0.5;
            control.extend(std::iter::repeat_n(sample, usize::from(channels)));
        }

        assert_oracle_load_bearing(&control, channels, sample_rate, 512);
    }

    #[kithara::test(native, flash(false))]
    fn loudness_fields_match_the_cochlea_probe() {
        let sample_rate = 48_000;
        let channels = 2;
        let frames = sample_rate as usize;
        let mut samples = Vec::with_capacity(frames * usize::from(channels));
        for frame in 0..frames {
            let phase = std::f32::consts::TAU * 997.0 * frame as f32 / sample_rate as f32;
            let sample = phase.sin() * 0.25;
            samples.extend(std::iter::repeat_n(sample, usize::from(channels)));
        }
        let actual = CochleaReport::measure(&samples, channels, sample_rate);
        let expected = probe(
            &Audio {
                samples,
                channels,
                sample_rate,
            },
            &ProbeOpts::default(),
        );

        assert_eq!(actual.integrated_lufs, expected.loudness.integrated_lufs);
        assert_eq!(
            actual.momentary_max_lufs,
            expected.loudness.momentary_max_lufs
        );
        assert_eq!(actual.sample_peak_dbfs, expected.loudness.sample_peak_dbfs);
        assert_eq!(actual.true_peak_dbtp, expected.loudness.true_peak_dbtp);
    }
}
