use std::ops::Range;

use cochlea_features::{
    Audio, ProbeOpts, SegmentOpts, TempoOpts, estimate_tempo, probe, segment_timeline,
};
use num_traits::cast;
use serde::Serialize;

const BEAT_MARKER_RATIO: f32 = 0.65;
const BEATS_PER_BAR: usize = 4;
const DOWNBEAT_MARKER_RATIO: f32 = 0.9;
const MARKER_CLUSTER_MS: usize = 100;
const SECONDS_PER_MINUTE: f64 = 60.0;
const TEMPO_TOLERANCE_BPM: f64 = 0.5;
const WINDOW_MS: f64 = 5.0;

/// Two independently rendered PCM runs aligned by their pre-command signal.
#[derive(Debug)]
#[non_exhaustive]
pub struct AlignedPcm {
    pub candidate: Vec<f32>,
    pub command_frame: usize,
    pub control: Vec<f32>,
}

/// Align independently rendered runs without using any post-command samples.
#[must_use]
pub fn align_command_runs(
    candidate: &[f32],
    candidate_command_frame: usize,
    control: &[f32],
    control_command_frame: usize,
    channels: u16,
) -> AlignedPcm {
    const ALIGNMENT_FRAMES: usize = 4_096;
    const SAMPLE_STRIDE: usize = 8;

    let channels = usize::from(channels);
    assert!(channels > 0, "alignment needs at least one channel");
    assert!(
        candidate.len().is_multiple_of(channels),
        "candidate contains incomplete frames"
    );
    assert!(
        control.len().is_multiple_of(channels),
        "control contains incomplete frames"
    );
    let prefix = ALIGNMENT_FRAMES
        .min(candidate_command_frame.div_ceil(2))
        .min(control_command_frame.div_ceil(2));
    assert!(prefix > 0, "alignment needs pre-command PCM");
    let candidate_limit = candidate_command_frame - prefix;
    let mut anchor = (f64::NEG_INFINITY, 0);
    for start in (0..=candidate_limit).step_by(SAMPLE_STRIDE) {
        let energy = (0..prefix)
            .step_by(SAMPLE_STRIDE)
            .map(|frame| {
                let sample = f64::from(candidate[(start + frame) * channels]);
                sample * sample
            })
            .sum::<f64>();
        if energy >= anchor.0 {
            anchor = (energy, start);
        }
    }
    let candidate_anchor = anchor.1;
    let min_lag = -i64::try_from(candidate_anchor).expect("alignment anchor fits i64");
    let max_lag = i64::try_from(control_command_frame - prefix).expect("command frame fits i64")
        - i64::try_from(candidate_anchor).expect("alignment anchor fits i64");
    let mut best = (f64::INFINITY, i64::MAX, 0_i64);
    for lag in min_lag..=max_lag {
        let control_anchor = usize::try_from(
            i64::try_from(candidate_anchor).expect("alignment anchor fits i64") + lag,
        )
        .expect("control alignment anchor fits usize");
        let squared = (0..prefix)
            .step_by(SAMPLE_STRIDE)
            .map(|frame| {
                let candidate = candidate[(candidate_anchor + frame) * channels];
                let control = control[(control_anchor + frame) * channels];
                let delta = f64::from(candidate - control);
                delta * delta
            })
            .sum::<f64>();
        if squared < best.0 || (squared == best.0 && lag.abs() < best.1) {
            best = (squared, lag.abs(), lag);
        }
    }
    let lag = best.2;
    let candidate_start = usize::try_from((-lag).max(0)).expect("alignment lag fits usize");
    let control_start = usize::try_from(lag.max(0)).expect("alignment lag fits usize");
    let candidate_frames = candidate.len() / channels - candidate_start;
    let control_frames = control.len() / channels - control_start;
    let frames = candidate_frames.min(control_frames);
    AlignedPcm {
        candidate: candidate[candidate_start * channels..(candidate_start + frames) * channels]
            .to_vec(),
        command_frame: candidate_command_frame - candidate_start,
        control: control[control_start * channels..(control_start + frames) * channels].to_vec(),
    }
}

/// Return the first frame where two runs differ for a sustained frame span.
#[must_use]
pub fn first_sustained_delta(
    candidate: &[f32],
    control: &[f32],
    channels: u16,
    range: Range<usize>,
    threshold: f32,
    sustained_frames: usize,
) -> Option<usize> {
    let channels = usize::from(channels);
    assert!(channels > 0, "delta detection needs at least one channel");
    assert!(sustained_frames > 0, "delta detection needs a frame span");
    let deltas = candidate
        .chunks_exact(channels)
        .zip(control.chunks_exact(channels))
        .map(|(candidate, control)| {
            candidate
                .iter()
                .zip(control)
                .map(|(candidate, control)| (candidate - control).abs())
                .fold(0.0_f32, f32::max)
        });
    let mut run = 0;
    for (frame, delta) in deltas.enumerate() {
        if !range.contains(&frame) {
            continue;
        }
        if delta > threshold {
            run += 1;
            if run == sustained_frames {
                return Some(frame + 1 - sustained_frames);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Select a percentile from test-oracle samples after sorting them in place.
#[must_use]
pub fn percentile_f32(values: &mut [f32], numerator: usize, denominator: usize) -> f32 {
    assert!(!values.is_empty(), "percentile input must not be empty");
    values.sort_by(f32::total_cmp);
    let index = values.len().saturating_sub(1).saturating_mul(numerator) / denominator;
    values[index]
}

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
    cochlea_failures(label, candidate, control, true)
}

/// Compare invariant Cochlea fields during active stretching; granular onset
/// changes are excluded and must be guarded by a separate frame-level oracle.
#[must_use]
pub fn time_stretch_failures(
    label: &str,
    candidate: &CochleaReport,
    control: &CochleaReport,
) -> Vec<String> {
    cochlea_failures(label, candidate, control, false)
}

/// Validate tempo and exact beat phase across deterministic rhythmic stems.
#[must_use]
pub fn synchronization_failures(
    label: &str,
    tracks: &[&[f32]],
    channels: u16,
    sample_rate: u32,
    target_bpm: f64,
) -> Vec<String> {
    assert!(channels > 0, "synchronization oracle needs a channel");
    assert!(
        sample_rate > 0,
        "synchronization oracle needs a sample rate"
    );
    assert!(
        target_bpm.is_finite() && target_bpm > 0.0,
        "synchronization oracle needs a positive finite BPM"
    );
    assert!(!tracks.is_empty(), "synchronization oracle needs a track");

    let mut failures = Vec::new();
    let channel_count = usize::from(channels);
    let beat_period: usize =
        cast((f64::from(sample_rate) * SECONDS_PER_MINUTE / target_bpm).round())
            .unwrap_or(1)
            .max(1);
    let mut phases = Vec::with_capacity(tracks.len());
    let mut bar_offsets = Vec::with_capacity(tracks.len());
    for (index, &samples) in tracks.iter().enumerate() {
        assert!(
            samples.len().is_multiple_of(channel_count),
            "track {index} must contain complete frames"
        );

        let tempo = estimate_tempo(
            &Audio {
                samples: samples.to_vec(),
                channels,
                sample_rate,
            },
            &TempoOpts::default(),
        );
        match tempo.bpm {
            Some(actual) if (actual - target_bpm).abs() <= TEMPO_TOLERANCE_BPM => {}
            Some(actual) => failures.push(format!(
                "{label}: track {index} tempo is {actual:.3} BPM, expected {target_bpm:.3} +/- {TEMPO_TOLERANCE_BPM:.3}",
            )),
            None => failures.push(format!("{label}: track {index} has no detected tempo")),
        }
        if !tempo.clear_rhythm {
            failures.push(format!(
                "{label}: track {index} has no clear rhythm: confidence={:.6}",
                tempo.confidence,
            ));
        }

        let (markers, downbeats) = rhythm_markers(samples, channel_count, sample_rate);
        let Some(&first) = markers.first() else {
            failures.push(format!("{label}: track {index} has no exact beat markers"));
            continue;
        };
        let marker_period = markers
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .min()
            .unwrap_or(beat_period);
        if let Some(pair) = markers
            .windows(2)
            .find(|pair| pair[1] - pair[0] == marker_period.saturating_mul(2))
        {
            failures.push(format!(
                "{label}: track {index} is missing a rhythmic event before frame {}",
                pair[1],
            ));
        }
        phases.push(first % beat_period);

        let Some(&first_downbeat) = downbeats.first() else {
            failures.push(format!(
                "{label}: track {index} has no exact downbeat markers"
            ));
            continue;
        };
        bar_offsets.push((first_downbeat - first) / marker_period % BEATS_PER_BAR);
    }

    if phases.len() == tracks.len() {
        let spread = circular_spread(&mut phases, beat_period);
        if spread > 0 {
            let suffix = if spread == 1 { "" } else { "s" };
            failures.push(format!(
                "{label}: beat phase spread is {spread} frame{suffix}",
            ));
        }
    }
    if bar_offsets.len() == tracks.len() {
        let spread = circular_spread(&mut bar_offsets, BEATS_PER_BAR);
        if spread > 0 {
            let suffix = if spread == 1 { "" } else { "s" };
            failures.push(format!(
                "{label}: bar phase spread is {spread} beat{suffix}",
            ));
        }
    }
    failures
}

fn rhythm_markers(samples: &[f32], channels: usize, sample_rate: u32) -> (Vec<usize>, Vec<usize>) {
    let track_peak = samples
        .chunks_exact(channels)
        .map(|values| values.iter().map(|sample| sample.abs()).fold(0.0, f32::max))
        .fold(0.0, f32::max);
    if track_peak <= f32::EPSILON {
        return (Vec::new(), Vec::new());
    }

    let threshold = track_peak * BEAT_MARKER_RATIO;
    let cluster_gap =
        usize::try_from(sample_rate).expect("sample rate fits usize") * MARKER_CLUSTER_MS / 1_000;
    let mut markers = Vec::new();
    let mut active: Option<(usize, f32, usize)> = None;
    for (frame, values) in samples.chunks_exact(channels).enumerate() {
        let peak = values.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
        if peak < threshold {
            continue;
        }
        match active {
            Some((best_frame, best_peak, last_frame)) if frame - last_frame <= cluster_gap => {
                active = Some(if peak > best_peak {
                    (frame, peak, frame)
                } else {
                    (best_frame, best_peak, frame)
                });
            }
            Some((best_frame, best_peak, _)) => {
                markers.push((best_frame, best_peak));
                active = Some((frame, peak, frame));
            }
            None => active = Some((frame, peak, frame)),
        }
    }
    if let Some((frame, peak, _)) = active {
        markers.push((frame, peak));
    }

    let downbeat_threshold = track_peak * DOWNBEAT_MARKER_RATIO;
    let downbeats = markers
        .iter()
        .filter_map(|(frame, peak)| (*peak >= downbeat_threshold).then_some(*frame))
        .collect();
    let beats = markers.into_iter().map(|(frame, _)| frame).collect();
    (beats, downbeats)
}

fn circular_spread(phases: &mut [usize], period: usize) -> usize {
    phases.sort_unstable();
    let inner_gap = phases
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .max()
        .unwrap_or(0);
    let wrap_gap = period - phases[phases.len() - 1] + phases[0];
    period - inner_gap.max(wrap_gap)
}

fn cochlea_failures(
    label: &str,
    candidate: &CochleaReport,
    control: &CochleaReport,
    compare_onsets: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    if candidate.silent_segments > control.silent_segments {
        failures.push(format!(
            "{label}: extra silent segments: candidate={}, control={}",
            candidate.silent_segments, control.silent_segments,
        ));
    }
    if compare_onsets && candidate.onset_count() != control.onset_count() {
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

    #[kithara::test(native, flash(false))]
    fn sustained_delta_requires_the_complete_span_and_reports_its_first_frame() {
        const CHANNELS: u16 = 2;
        const FRAMES: usize = 512;
        const THRESHOLD: f32 = 0.002;
        const SUSTAINED: usize = 32;

        let control = vec![0.0; FRAMES * usize::from(CHANNELS)];
        assert_eq!(
            first_sustained_delta(
                &control,
                &control,
                CHANNELS,
                0..FRAMES,
                THRESHOLD,
                SUSTAINED
            ),
            None
        );

        let mut candidate = control.clone();
        let onset = 442;
        let short_end = onset + SUSTAINED - 1;
        candidate[onset * usize::from(CHANNELS)..short_end * usize::from(CHANNELS)].fill(0.5);
        assert_eq!(
            first_sustained_delta(
                &candidate,
                &control,
                CHANNELS,
                0..FRAMES,
                THRESHOLD,
                SUSTAINED,
            ),
            None
        );

        candidate[short_end * usize::from(CHANNELS)..(short_end + 1) * usize::from(CHANNELS)]
            .fill(0.5);
        assert_eq!(
            first_sustained_delta(
                &candidate,
                &control,
                CHANNELS,
                0..FRAMES,
                THRESHOLD,
                SUSTAINED,
            ),
            Some(onset)
        );
        assert!(onset > 441, "frame 442 is outside a 441-frame budget");
    }
}
