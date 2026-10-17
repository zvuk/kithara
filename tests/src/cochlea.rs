use cochlea_features::{
    Audio, ProbeOpts, SegmentOpts, TempoOpts, TempoReport, estimate_tempo, probe, segment_timeline,
};
use serde::Serialize;

const BEAT_MARKER_RATIO: f32 = 0.65;
const BEATS_PER_BAR: usize = 4;
const DOWNBEAT_MARKER_RATIO: f32 = 0.9;
const MARKER_CLUSTER_MS: usize = 100;
const ONSET_MATCH_MS: f64 = 20.0;
const ONSET_NORMALIZATION_MS: f32 = 20.0;
const NORMALIZED_ONSET_RMS: f32 = 20.0;
const PERIOD_SEARCH_FRACTION: f64 = 0.05;
const TEMPO_BANDS: [(f64, f64); 4] = [(30.0, 60.0), (60.0, 120.0), (120.0, 240.0), (240.0, 300.0)];
const SECONDS_PER_MINUTE: f64 = 60.0;
const TEMPO_TOLERANCE_BPM: f64 = 0.5;
const WINDOW_MS: f64 = 5.0;

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

/// Require a mix to retain the loudness of its loudest contributing deck.
#[must_use]
pub fn mix_loudness_failures(
    label: &str,
    mix: &CochleaReport,
    decks: &[CochleaReport],
    tolerance_lu: f64,
) -> Vec<String> {
    assert!(!decks.is_empty(), "mix loudness needs at least one deck");
    assert!(
        tolerance_lu.is_finite() && tolerance_lu >= 0.0,
        "mix loudness tolerance must be finite and non-negative"
    );

    let mut failures = Vec::new();
    let Some(mix_lufs) = mix.integrated_lufs else {
        failures.push(format!("{label}: mix integrated loudness is undefined"));
        return failures;
    };
    let mut loudest = f64::NEG_INFINITY;
    for (index, deck) in decks.iter().enumerate() {
        match deck.integrated_lufs {
            Some(lufs) => loudest = loudest.max(lufs),
            None => failures.push(format!(
                "{label}: deck {index} integrated loudness is undefined"
            )),
        }
    }
    if loudest.is_finite() && mix_lufs + tolerance_lu < loudest {
        failures.push(format!(
            "{label}: mix is quieter than the loudest deck: mix={mix_lufs:.3} LUFS, loudest={loudest:.3} LUFS, tolerance={tolerance_lu:.3} LU"
        ));
    }
    failures
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
    synchronization_failures_with(label, tracks, channels, sample_rate, target_bpm, true)
}

/// Validate deterministic score fixtures from their exact beat markers.
#[must_use]
pub fn marked_synchronization_failures(
    label: &str,
    tracks: &[&[f32]],
    channels: u16,
    sample_rate: u32,
    target_bpm: f64,
) -> Vec<String> {
    synchronization_failures_with(label, tracks, channels, sample_rate, target_bpm, false)
}

/// Extract deterministic fixture beat and downbeat markers with the calibrated
/// score-marker detector used by the synchronization oracle.
///
/// A first cluster within one cluster window of the buffer start, with no silent
/// analysis window before it, is the body of an event that began before the
/// capture and carries no position; it is dropped rather than read as a beat.
#[must_use]
pub fn marked_rhythm_markers(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> (Vec<usize>, Vec<usize>) {
    rhythm_markers(samples, usize::from(channels), sample_rate)
}

/// Every exact fixture beat marker in `track` that starts off its nearest Host beat.
///
/// `host_beats` are frames of the authoritative Host grid inside the same capture,
/// so a deck that keeps its phase with other decks but trails the Host still fails.
#[must_use]
pub fn host_beat_alignment_failures(
    label: &str,
    track: &[f32],
    channels: u16,
    sample_rate: u32,
    host_beats: &[usize],
) -> Vec<String> {
    if host_beats.is_empty() {
        return vec![format!("{label}: capture holds no Host beats")];
    }
    let (markers, _) = marked_rhythm_markers(track, channels, sample_rate);
    if markers.is_empty() {
        return vec![format!("{label}: capture holds no exact beat markers")];
    }
    markers
        .into_iter()
        .filter_map(|marker| {
            let beat = *host_beats
                .iter()
                .min_by_key(|beat| beat.abs_diff(marker))
                .expect("Host beats are not empty");
            (marker != beat).then(|| {
                let offset = marker as i64 - beat as i64;
                format!(
                    "{label}: beat marker at frame {marker} is {offset:+} frames from Host beat at frame {beat}"
                )
            })
        })
        .collect()
}

fn synchronization_failures_with(
    label: &str,
    tracks: &[&[f32]],
    channels: u16,
    sample_rate: u32,
    target_bpm: f64,
    estimate: bool,
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
    let mut beat_period = f64::from(sample_rate) * SECONDS_PER_MINUTE / target_bpm;
    let mut phases = Vec::with_capacity(tracks.len());
    let mut bar_offsets = Vec::with_capacity(tracks.len());
    for (index, &samples) in tracks.iter().enumerate() {
        assert!(
            samples.len().is_multiple_of(channel_count),
            "track {index} must contain complete frames"
        );

        let (mut markers, mut downbeats) = marked_rhythm_markers(samples, channels, sample_rate);
        let marker_debug = markers.iter().take(8).copied().collect::<Vec<_>>();
        let marker_period = markers.windows(2).map(|pair| pair[1] - pair[0]).min();
        let mut exact_lattice = !estimate;
        if estimate {
            let Some((mut actual, tempo)) = estimated_rhythm(samples, channels, sample_rate) else {
                failures.push(format!("{label}: track {index} has no exact beat markers"));
                continue;
            };
            let period = f64::from(sample_rate) * SECONDS_PER_MINUTE / actual;
            exact_lattice = marker_period.is_some_and(|marker_period| {
                let marker_period = marker_period as f64;
                (marker_period - period).abs() < period * PERIOD_SEARCH_FRACTION
                    && markers.windows(2).all(|pair| {
                        let gap = (pair[1] - pair[0]) as f64;
                        let beats = (gap / marker_period).round();
                        beats >= 1.0 && (gap - beats * marker_period).abs() <= beats
                    })
            });
            if exact_lattice && let Some(marker_period) = marker_period {
                let beats = markers
                    .windows(2)
                    .map(|pair| ((pair[1] - pair[0]) as f64 / marker_period as f64).round())
                    .sum::<f64>();
                let span = (markers[markers.len() - 1] - markers[0]) as f64;
                actual = f64::from(sample_rate) * SECONDS_PER_MINUTE * beats / span;
            }
            if index == 0 {
                beat_period = f64::from(sample_rate) * SECONDS_PER_MINUTE / actual;
            }
            if (actual - target_bpm).abs() > TEMPO_TOLERANCE_BPM {
                failures.push(format!(
                    "{label}: track {index} estimated tempo is {actual:.3} BPM, expected {target_bpm:.3} +/- {TEMPO_TOLERANCE_BPM:.3}; markers={marker_debug:?}",
                ));
            }
            if !exact_lattice {
                let envelope = rms_envelope(samples, channel_count, sample_rate);
                markers = localized_grid(&tempo.beats_ms, &envelope, sample_rate);
                downbeats = localized_grid(&tempo.downbeats_ms, &envelope, sample_rate);
            }
        } else {
            if markers.is_empty() {
                failures.push(format!("{label}: track {index} has no exact beat markers"));
                continue;
            }
            let Some(marker_period) = marker_period else {
                failures.push(format!("{label}: track {index} has no detected tempo"));
                continue;
            };
            let marker_bpm = f64::from(sample_rate) * SECONDS_PER_MINUTE / marker_period as f64;
            if (marker_bpm - target_bpm).abs() > TEMPO_TOLERANCE_BPM {
                failures.push(format!(
                    "{label}: track {index} tempo is {marker_bpm:.3} BPM, expected {target_bpm:.3} +/- {TEMPO_TOLERANCE_BPM:.3}; markers={marker_debug:?}",
                ));
            }
        }
        if exact_lattice
            && let Some(marker_period) = marker_period
            && let Some(pair) = markers
                .windows(2)
                .find(|pair| (pair[1] - pair[0]) as f64 / marker_period as f64 >= 1.5)
        {
            failures.push(format!(
                "{label}: track {index} is missing a rhythmic event before frame {}",
                pair[1],
            ));
        }
        phases.push(markers);

        if downbeats.is_empty() {
            failures.push(format!(
                "{label}: track {index} has no exact downbeat markers"
            ));
            continue;
        }
        bar_offsets.push(downbeats);
    }

    if phases.len() == tracks.len() {
        let Some(spread) = phase_spread(&phases, beat_period) else {
            failures.push(format!(
                "{label}: tracks have no common beat phase reference"
            ));
            return failures;
        };
        let spread = spread.round() as usize;
        if spread > 0 {
            let suffix = if spread == 1 { "" } else { "s" };
            failures.push(format!(
                "{label}: beat phase spread is {spread} frame{suffix}",
            ));
        }
    }
    if bar_offsets.len() == tracks.len() {
        let Some(spread) = phase_spread(&bar_offsets, beat_period * BEATS_PER_BAR as f64) else {
            failures.push(format!(
                "{label}: tracks have no common downbeat phase reference"
            ));
            return failures;
        };
        let spread = (spread / beat_period).round() as usize;
        if spread > 0 {
            let suffix = if spread == 1 { "" } else { "s" };
            failures.push(format!(
                "{label}: bar phase spread is {spread} beat{suffix}",
            ));
        }
    }
    failures
}

fn refined_tempo(tempo: &TempoReport) -> Option<f64> {
    tempo.bpm?;
    if tempo.beats_ms.len() < 2 {
        return None;
    }
    let center = (tempo.beats_ms.len() - 1) as f64 / 2.0;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for (index, timestamp) in tempo.beats_ms.iter().enumerate() {
        let centered_index = index as f64 - center;
        covariance += centered_index * timestamp;
        variance += centered_index * centered_index;
    }
    let period_ms = covariance / variance;
    (period_ms.is_finite() && period_ms > 0.0).then(|| SECONDS_PER_MINUTE * 1_000.0 / period_ms)
}

fn onset_audio(samples: &[f32], channels: u16, sample_rate: u32) -> Audio {
    let mut normalized = samples.to_vec();
    let gain = 1.0 - (-1_000.0 / (ONSET_NORMALIZATION_MS * sample_rate as f32)).exp();
    let mut power = 0.0;
    for frame in normalized.chunks_exact_mut(usize::from(channels)) {
        let energy = frame.iter().map(|sample| sample * sample).sum::<f32>() / f32::from(channels);
        power += gain * (energy - power);
        for sample in frame {
            *sample *= NORMALIZED_ONSET_RMS / power.max(1e-5).sqrt();
        }
    }
    Audio {
        samples: normalized,
        channels,
        sample_rate,
    }
}

fn estimated_rhythm(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Option<(f64, TempoReport)> {
    let audio = onset_audio(samples, channels, sample_rate);
    let onsets = probe(&audio, &ProbeOpts::default()).onsets.times_ms;
    if onsets.len() < 2 {
        return None;
    }
    let envelope = rms_envelope(samples, usize::from(channels), sample_rate);
    let mut best = None;
    let mut best_score = 0.0;
    for (min_bpm, max_bpm) in TEMPO_BANDS {
        let band = estimate_tempo(
            &audio,
            &TempoOpts::default()
                .with_min_bpm(min_bpm)
                .with_max_bpm(max_bpm),
        );
        for candidate in band.candidates {
            let grid = estimate_tempo(
                &audio,
                &TempoOpts::default()
                    .with_min_bpm(candidate.bpm * 0.98)
                    .with_max_bpm(candidate.bpm * 1.02),
            );
            let matches = onsets
                .iter()
                .filter(|onset| {
                    grid.beats_ms
                        .iter()
                        .any(|beat| (beat - **onset).abs() < ONSET_MATCH_MS)
                })
                .count();
            let coverage = matches as f64 / onsets.len() as f64;
            let Some(rough_bpm) = refined_tempo(&grid) else {
                continue;
            };
            let Some((bpm, correlation)) = refine_period(&envelope, sample_rate, rough_bpm) else {
                continue;
            };
            let score = candidate.salience * coverage * coverage * correlation.max(0.0);
            if score > best_score {
                best_score = score;
                best = Some((bpm, grid));
            }
        }
    }
    best
}

fn rms_envelope(samples: &[f32], channels: usize, sample_rate: u32) -> Vec<f64> {
    let gain = 1.0 - (-1_000.0 / f64::from(sample_rate)).exp();
    let mut power = 0.0;
    samples
        .chunks_exact(channels)
        .map(|frame| {
            let energy = frame
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / channels as f64;
            power += gain * (energy - power);
            power.max(0.0).sqrt()
        })
        .collect()
}

fn envelope_correlation(
    envelope: &[f64],
    lag: usize,
    frames: usize,
    step: usize,
    mean: f64,
) -> f64 {
    let mut product = 0.0;
    let mut left_power = 0.0;
    let mut right_power = 0.0;
    for frame in (0..frames).step_by(step) {
        let left = envelope[frame] - mean;
        let right = envelope[frame + lag] - mean;
        product += left * right;
        left_power += left * left;
        right_power += right * right;
    }
    product / (left_power * right_power).sqrt()
}

fn refine_period(envelope: &[f64], sample_rate: u32, bpm: f64) -> Option<(f64, f64)> {
    let center = (f64::from(sample_rate) * SECONDS_PER_MINUTE * 2.0 / bpm).round() as usize;
    let radius = (center as f64 * PERIOD_SEARCH_FRACTION).ceil() as usize;
    let step = (sample_rate as usize / 1_000).max(1);
    let frames = envelope.len().checked_sub(center + radius + step + 1)?;
    let mean = envelope.iter().sum::<f64>() / envelope.len() as f64;
    let score = |lag| envelope_correlation(envelope, lag, frames, step, mean);
    let mut best = center;
    let mut best_score = f64::NEG_INFINITY;
    for lag in (center.saturating_sub(radius)..=center + radius).step_by(step) {
        let correlation = score(lag);
        if correlation > best_score {
            best = lag;
            best_score = correlation;
        }
    }
    let coarse = best;
    for lag in coarse.saturating_sub(step).max(1)..=coarse + step {
        let correlation = score(lag);
        if correlation > best_score {
            best = lag;
            best_score = correlation;
        }
    }
    let left = score(best - 1);
    let right = score(best + 1);
    let curvature = left - 2.0 * best_score + right;
    if !curvature.is_finite() || curvature >= 0.0 {
        return None;
    }
    let lag = best as f64 + (0.5 * (left - right) / curvature).clamp(-1.0, 1.0);
    Some((
        f64::from(sample_rate) * SECONDS_PER_MINUTE * 2.0 / lag,
        best_score,
    ))
}

fn localized_grid(grid: &[f64], envelope: &[f64], sample_rate: u32) -> Vec<usize> {
    let radius = (ONSET_MATCH_MS * f64::from(sample_rate) / 1_000.0) as usize;
    grid.iter()
        .filter_map(|timestamp| {
            let center = (timestamp * f64::from(sample_rate) / 1_000.0).round() as usize;
            let start = center.saturating_sub(radius).max(1);
            let end = (center + radius).min(envelope.len());
            (start..end).max_by(|left, right| {
                (envelope[*left] - envelope[*left - 1])
                    .total_cmp(&(envelope[*right] - envelope[*right - 1]))
            })
        })
        .collect()
}

/// Whether any analysis window before `frame` sits at or below Cochlea's silence floor.
///
/// A window, not a sample: decoded media crosses exact zero on ordinary waveform
/// crossings, so a single zero frame says nothing about whether a gap preceded an onset.
fn lead_in_has_silence(samples: &[f32], channels: usize, sample_rate: u32, frame: usize) -> bool {
    let Ok(channel_count) = u16::try_from(channels) else {
        return false;
    };
    let lead_in = &samples[..(frame * channels).min(samples.len())];
    if lead_in.is_empty() {
        return false;
    }
    segment_timeline(
        &Audio {
            samples: lead_in.to_vec(),
            channels: channel_count,
            sample_rate,
        },
        &SegmentOpts::default().with_window_ms(WINDOW_MS),
    )
    .segments
    .iter()
    .any(|segment| segment.silent)
}

struct RhythmCluster {
    first: usize,
    peak_frame: usize,
    peak: f32,
    last: usize,
}

/// Cluster loud frames and locate each marker at its maximum amplitude.
///
/// The leading cluster is dropped when no silent window precedes it: within one
/// cluster window of the start an onset and the body of an event that began
/// before the capture look the same, and reading such a fragment as a beat moves
/// one deck's phase by its length while a quieter deck keeps its real beat.
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
    let mut active: Option<RhythmCluster> = None;
    for (frame, values) in samples.chunks_exact(channels).enumerate() {
        let peak = values.iter().map(|sample| sample.abs()).fold(0.0, f32::max);
        if peak < threshold {
            continue;
        }
        match active.as_mut() {
            Some(cluster) if frame - cluster.last <= cluster_gap => {
                if peak > cluster.peak {
                    cluster.peak_frame = frame;
                    cluster.peak = peak;
                }
                cluster.last = frame;
            }
            _ => {
                if let Some(cluster) = active.take() {
                    markers.push(cluster);
                }
                active = Some(RhythmCluster {
                    first: frame,
                    peak_frame: frame,
                    peak,
                    last: frame,
                });
            }
        }
    }
    if let Some(cluster) = active {
        markers.push(cluster);
    }

    let truncated_body = markers.first().is_some_and(|cluster| {
        cluster.first < cluster_gap
            && !lead_in_has_silence(samples, channels, sample_rate, cluster.first)
    });
    if truncated_body {
        markers.remove(0);
    }

    let downbeat_threshold = track_peak * DOWNBEAT_MARKER_RATIO;
    let downbeats = markers
        .iter()
        .filter_map(|cluster| (cluster.peak >= downbeat_threshold).then_some(cluster.peak_frame))
        .collect();
    let beats = markers
        .into_iter()
        .map(|cluster| cluster.peak_frame)
        .collect();
    (beats, downbeats)
}

fn phase_spread(events: &[Vec<usize>], period: f64) -> Option<f64> {
    let start = events.iter().map(|frames| frames[0]).max().unwrap_or(0);
    let end = events
        .iter()
        .map(|frames| frames[frames.len() - 1])
        .min()
        .unwrap_or(start);
    events[0]
        .iter()
        .copied()
        .filter(|reference| (start..=end).contains(reference))
        .map(|reference| {
            let mut phases = events
                .iter()
                .map(|frames| {
                    let frame = frames
                        .iter()
                        .min_by_key(|frame| frame.abs_diff(reference))
                        .map_or(reference, |frame| *frame);
                    (frame as f64 - reference as f64).rem_euclid(period)
                })
                .collect::<Vec<_>>();
            phases.sort_by(f64::total_cmp);
            let wrap_gap = period - phases[phases.len() - 1] + phases[0];
            let gap = phases
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .fold(wrap_gap, f64::max);
            period - gap
        })
        .reduce(f64::max)
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
        failures.push(format!(
            "{label}: candidate-only true peak over 0 dBTP: candidate={:?} dBTP/{:?} dBFS, control={:?} dBTP/{:?} dBFS",
            candidate.true_peak_dbtp,
            candidate.sample_peak_dbfs,
            control.true_peak_dbtp,
            control.sample_peak_dbfs
        ));
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
    use kithara_test_fixtures::integration_fixtures::{cochlea_control, cochlea_loudness};
    use kithara_test_utils::kithara;

    use super::*;

    fn pulse_train(sample_rate: u32, bpm: f64) -> Vec<f32> {
        let beat_frames = f64::from(sample_rate) * SECONDS_PER_MINUTE / bpm;
        let leading_frames = sample_rate as usize / 4;
        let pulse_frames = sample_rate as usize / 50;
        let frame_count = leading_frames + (beat_frames * 6.0).ceil() as usize;
        let mut samples = vec![0.0; frame_count];
        for beat in 0..6 {
            let start = leading_frames + (beat as f64 * beat_frames).round() as usize;
            let amplitude = if beat % BEATS_PER_BAR == 0 { 0.9 } else { 0.7 };
            for offset in 0..pulse_frames {
                let phase = std::f32::consts::TAU * 1_000.0 * offset as f32 / sample_rate as f32;
                samples[start + offset] =
                    amplitude * phase.cos() * (1.0 - offset as f32 / pulse_frames as f32);
            }
        }
        samples
    }

    #[kithara::test(native)]
    #[case::cd_rate(44_100)]
    #[case::render_rate(48_000)]
    fn marker_localization_keeps_the_peak_under_preringing_and_gain(#[case] sample_rate: u32) {
        let original = pulse_train(sample_rate, 120.0);
        let (expected_beats, expected_downbeats) = marked_rhythm_markers(&original, 1, sample_rate);
        assert_eq!(expected_beats.len(), 6);
        assert_eq!(expected_downbeats.len(), 2);
        let mut preringing = original.clone();
        for &beat in &expected_beats {
            preringing[beat - 1] = original[beat] * 0.9;
        }
        for sample in &mut preringing {
            *sample *= 0.25;
        }
        assert!(preringing[expected_beats[0] - 1] > 0.9 * 0.25 * BEAT_MARKER_RATIO);
        assert_eq!(
            marked_rhythm_markers(&preringing, 1, sample_rate),
            (expected_beats.clone(), expected_downbeats),
        );
        assert!(
            marked_synchronization_failures(
                "aligned pulse peaks",
                &[&original, &preringing],
                1,
                sample_rate,
                120.0,
            )
            .is_empty()
        );
        assert!(
            host_beat_alignment_failures(
                "aligned Host peaks",
                &preringing,
                1,
                sample_rate,
                &expected_beats,
            )
            .is_empty()
        );
    }

    #[kithara::test(native)]
    #[case::cd_rate(44_100)]
    #[case::render_rate(48_000)]
    fn marker_localization_rejects_a_frame_shift_and_a_downbeat_shift(#[case] sample_rate: u32) {
        let original = pulse_train(sample_rate, 120.0);
        let (beats, _) = marked_rhythm_markers(&original, 1, sample_rate);
        let frame_shifted = [vec![0.0], original.clone()].concat();
        let frame_failures = marked_synchronization_failures(
            "one frame",
            &[&original, &frame_shifted],
            1,
            sample_rate,
            120.0,
        );
        assert!(
            frame_failures
                .iter()
                .any(|failure| failure == "one frame: beat phase spread is 1 frame"),
            "{frame_failures:?}"
        );
        let host_failures =
            host_beat_alignment_failures("one Host frame", &frame_shifted, 1, sample_rate, &beats);
        assert_eq!(host_failures.len(), beats.len());
        assert!(
            host_failures
                .iter()
                .all(|failure| failure.contains("is +1 frames from Host beat")),
            "{host_failures:?}"
        );

        let beat_frames = sample_rate as usize / 2;
        let bar_shifted = [vec![0.0; beat_frames], original].concat();
        let bar_failures = marked_synchronization_failures(
            "one downbeat",
            &[&pulse_train(sample_rate, 120.0), &bar_shifted],
            1,
            sample_rate,
            120.0,
        );
        assert!(
            bar_failures
                .iter()
                .any(|failure| failure == "one downbeat: bar phase spread is 1 beat"),
            "{bar_failures:?}"
        );
    }

    #[kithara::test(native)]
    #[case::cd_rate(44_100)]
    #[case::render_rate(48_000)]
    fn marker_localization_preserves_native_offsets_and_rejects_a_dropped_pulse(
        #[case] sample_rate: u32,
    ) {
        let original = pulse_train(sample_rate, 120.0);
        let lead = sample_rate as usize / 4;
        let period = sample_rate as usize / 2;
        let (beats, downbeats) = marked_rhythm_markers(&original, 1, sample_rate);
        assert_eq!(
            beats,
            (0..6).map(|beat| lead + beat * period).collect::<Vec<_>>()
        );
        assert_eq!(downbeats, [lead, lead + 4 * period]);
        let mut missing = original.clone();
        missing[beats[2]..beats[2] + sample_rate as usize / 50].fill(0.0);
        let failures = marked_synchronization_failures(
            "dropped pulse",
            &[&original, &missing],
            1,
            sample_rate,
            120.0,
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("track 1 is missing a rhythmic event")),
            "{failures:?}"
        );
    }

    #[kithara::test(native)]
    #[case::cd_rate(44_100)]
    #[case::render_rate(48_000)]
    fn tempo_oracle_resolves_127_bpm_between_onset_bins(#[case] sample_rate: u32) {
        let samples = pulse_train(sample_rate, 127.0);
        let failures = synchronization_failures(
            "fractional onset period",
            &[&samples],
            1,
            sample_rate,
            127.0,
        );
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[kithara::test(native)]
    #[case::cd_slow(44_100, 126.0)]
    #[case::cd_fast(44_100, 128.0)]
    #[case::render_slow(48_000, 126.0)]
    #[case::render_fast(48_000, 128.0)]
    #[case::half_tempo(48_000, 63.5)]
    #[case::cd_just_slow(44_100, 126.49)]
    #[case::cd_just_fast(44_100, 127.51)]
    #[case::render_just_slow(48_000, 126.49)]
    #[case::render_just_fast(48_000, 127.51)]
    fn tempo_oracle_rejects_real_drift_and_the_wrong_octave(
        #[case] sample_rate: u32,
        #[case] actual_bpm: f64,
    ) {
        let samples = pulse_train(sample_rate, actual_bpm);
        let failures =
            synchronization_failures("wrong PCM tempo", &[&samples], 1, sample_rate, 127.0);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("estimated tempo is"), "{failures:?}");
    }

    #[kithara::test(native)]
    fn tempo_oracle_rejects_silence() {
        let samples = vec![0.0; 48_000 * 3];
        assert_eq!(
            synchronization_failures("silence", &[&samples], 1, 48_000, 127.0),
            ["silence: track 0 has no exact beat markers"],
        );
    }

    #[kithara::test(native)]
    #[case::cd_rate(44_100)]
    #[case::render_rate(48_000)]
    fn synchronization_rejects_missing_irregular_and_accumulating_pulses(#[case] sample_rate: u32) {
        let complete = pulse_train(sample_rate, 127.0);
        let period = f64::from(sample_rate) * SECONDS_PER_MINUTE / 127.0;
        let lead = sample_rate as usize / 4;
        let width = sample_rate as usize / 50;
        for perturbation in 0..3 {
            let mut changed = vec![0.0; complete.len()];
            for beat in 0..6 {
                let start = lead + (beat as f64 * period).round() as usize;
                if perturbation == 0 && beat == 3 {
                    continue;
                }
                let offset = match perturbation {
                    1 if beat == 3 => sample_rate as usize / 500,
                    2 => beat * beat * sample_rate as usize / 2_000,
                    _ => 0,
                };
                changed[start + offset..start + offset + width]
                    .copy_from_slice(&complete[start..start + width]);
            }
            let failures = synchronization_failures(
                "irregular control",
                &[&complete, &changed],
                1,
                sample_rate,
                127.0,
            );
            assert!(
                !failures.is_empty(),
                "perturbation {perturbation} at {sample_rate} was accepted"
            );
        }
    }

    #[kithara::test(native)]
    fn phase_oracle_uses_a_common_frame_despite_different_retained_markers() {
        let sample_rate = 44_100;
        let complete = pulse_train(sample_rate, 127.0);
        let mut retained = complete.clone();
        retained[..sample_rate as usize / 2].fill(0.0);
        assert!(
            synchronization_failures(
                "retained markers",
                &[&complete, &retained],
                1,
                sample_rate,
                127.0
            )
            .is_empty()
        );
        let shifted = [vec![0.0], retained].concat();
        assert_eq!(
            synchronization_failures(
                "shifted markers",
                &[&complete, &shifted],
                1,
                sample_rate,
                127.0
            ),
            [
                "shifted markers: beat phase spread is 1 frame",
                "shifted markers: tracks have no common downbeat phase reference",
            ]
        );
    }

    #[kithara::test(native)]
    fn phase_oracle_rejects_disjoint_regular_markers() {
        let sample_rate = 48_000;
        let complete = pulse_train(sample_rate, 120.0);
        let shifted = [vec![0.0; sample_rate as usize * 4 + 1], complete.clone()].concat();
        assert_eq!(
            marked_synchronization_failures(
                "disjoint markers",
                &[&complete, &shifted],
                1,
                sample_rate,
                120.0,
            ),
            ["disjoint markers: tracks have no common beat phase reference"]
        );
    }

    #[kithara::test(native, flash(false))]
    fn comparator_rejects_one_missing_quantum_and_one_clipped_frame(cochlea_control: Vec<f32>) {
        let sample_rate = 48_000;
        let channels = 2;
        let control = cochlea_control;
        assert_oracle_load_bearing(&control, channels, sample_rate, 512);
    }

    #[kithara::test(native, flash(false))]
    fn loudness_fields_match_the_cochlea_probe(cochlea_loudness: Vec<f32>) {
        let sample_rate = 48_000;
        let channels = 2;
        let samples = cochlea_loudness;
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
