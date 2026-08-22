use kithara_integration_tests::cochlea::CochleaReport;
use serde::Serialize;

use super::{
    BLOCK_FRAMES, BOUNDARY_OUTLIER_RATIO, CHANNELS, EXACT_ZERO_RUN_LIMIT_FRAMES,
    MAX_DECK_GAIN_DELTA, MAX_MATCHED_RMS_DELTA_DB, MIN_BOUNDARY_JUMP, MIN_DECK_CONTRIBUTION_RATIO,
    MIN_FIXED_STEM_RMS_DBFS, MIX_HEADROOM,
};

#[derive(Serialize)]
pub(super) struct SampleContinuityReport {
    pub(super) discontinuity_boundaries: Vec<usize>,
    pub(super) longest_exact_zero_run_frames: usize,
    pub(super) max_boundary_jump: f32,
    pub(super) p99_adjacent_jump: f32,
    pub(super) repeated_block_boundaries: Vec<usize>,
}

pub(super) struct OracleReports {
    pub(super) cochlea: Option<CochleaReport>,
    pub(super) sample_continuity: Option<SampleContinuityReport>,
}

#[derive(Serialize)]
pub(super) struct AudioLevelReport {
    pub(super) label: String,
    pub(super) role: AudioRole,
    pub(super) rms_dbfs: Option<f64>,
    pub(super) peak_dbfs: Option<f64>,
    pub(super) integrated_lufs: Option<f64>,
    pub(super) momentary_max_lufs: Option<f64>,
    pub(super) sample_peak_dbfs: Option<f64>,
    pub(super) true_peak_dbtp: Option<f64>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum AudioRole {
    DirectReference,
    Contribution,
    ReferenceMix,
    FinalMix,
}

#[derive(Serialize)]
pub(super) struct MatchedMixReport {
    pub(super) candidate_rms_dbfs: Option<f64>,
    pub(super) reference_rms_dbfs: Option<f64>,
    pub(super) rms_delta_db: Option<f64>,
    pub(super) residual_ratio: f64,
    pub(super) deck_contribution_ratios: Vec<f64>,
    pub(super) deck_gain_estimates: Vec<f64>,
}

pub(super) struct MatchedMixAudio {
    pub(super) reference: Vec<f32>,
    pub(super) contributions: Vec<Vec<f32>>,
    pub(super) report: Option<MatchedMixReport>,
}

/// `no_block`: Cochlea analysis of the whole capture is offline measurement,
/// not a blocking wait; it is seconds of arithmetic per case and the test has
/// nothing to overlap it with.
#[kithara::allow_block]
pub(super) fn assess_audio(
    label: &str,
    sample_rate: u32,
    capture: &[f32],
    failures: &mut Vec<String>,
) -> OracleReports {
    let finite = capture.iter().all(|sample| sample.is_finite());
    let sample_continuity = finite.then(|| measure_sample_continuity(capture));
    if let Some(report) = &sample_continuity {
        if report.longest_exact_zero_run_frames >= EXACT_ZERO_RUN_LIMIT_FRAMES {
            failures.push(format!(
                "{}: final mix contained an exact-zero run of {} frames",
                label, report.longest_exact_zero_run_frames,
            ));
        }
        if !report.repeated_block_boundaries.is_empty() {
            failures.push(format!(
                "{}: final mix repeated callback blocks at frame boundaries {:?}",
                label, report.repeated_block_boundaries,
            ));
        }
        if !report.discontinuity_boundaries.is_empty() {
            failures.push(format!(
                "{}: final mix had callback-boundary jump outliers at frames {:?} (max={:.6}, adjacent_p99={:.6})",
                label,
                report.discontinuity_boundaries,
                report.max_boundary_jump,
                report.p99_adjacent_jump,
            ));
        }
    }

    let cochlea = finite.then(|| CochleaReport::measure(capture, CHANNELS, sample_rate));
    if let Some(report) = &cochlea {
        if report.clipped_samples > 0 || report.true_peak_over_0dbtp {
            failures.push(format!(
                "{}: conservative mix clipped (samples={}, true_peak_over_0dbtp={})",
                label, report.clipped_samples, report.true_peak_over_0dbtp,
            ));
        }
    } else {
        failures.push(format!(
            "{}: final PCM contained non-finite samples, so Cochlea could not analyse it",
            label,
        ));
    }

    OracleReports {
        cochlea,
        sample_continuity,
    }
}

pub(super) fn assess_matched_mix(
    label: &str,
    candidate: &[f32],
    stems: &[&[f32]],
    failures: &mut Vec<String>,
) -> MatchedMixAudio {
    if stems.is_empty() {
        failures.push(format!("{label}: matched mix has no deck stems"));
        return MatchedMixAudio {
            reference: Vec::new(),
            contributions: Vec::new(),
            report: None,
        };
    }
    if candidate.is_empty() || stems.iter().any(|stem| stem.len() != candidate.len()) {
        failures.push(format!(
            "{label}: matched mix shapes differ: candidate={}, stems={:?}",
            candidate.len(),
            stems.iter().map(|stem| stem.len()).collect::<Vec<_>>(),
        ));
        return MatchedMixAudio {
            reference: Vec::new(),
            contributions: Vec::new(),
            report: None,
        };
    }

    let deck_count = u16::try_from(stems.len()).expect("matrix deck count fits u16");
    let scale = MIX_HEADROOM / f32::from(deck_count);
    let contributions = stems
        .iter()
        .map(|stem| {
            stem.iter()
                .map(|sample| *sample * scale)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut reference = vec![0.0; candidate.len()];
    for contribution in &contributions {
        for (sample, source) in reference.iter_mut().zip(contribution.iter().copied()) {
            *sample += source;
        }
    }

    let reference_norm = l2_norm(&reference);
    if !reference_norm.is_finite() || reference_norm <= f64::EPSILON {
        failures.push(format!(
            "{label}: matched reference has no finite audible energy"
        ));
        return MatchedMixAudio {
            reference,
            contributions,
            report: None,
        };
    }
    let deck_contribution_ratios = stems
        .iter()
        .map(|stem| l2_norm_scaled(stem, scale) / reference_norm)
        .collect::<Vec<_>>();
    if deck_contribution_ratios
        .iter()
        .any(|ratio| !ratio.is_finite() || *ratio <= MIN_DECK_CONTRIBUTION_RATIO)
    {
        failures.push(format!(
            "{label}: fixed capture window cannot prove every deck contribution: {deck_contribution_ratios:?}",
        ));
        return MatchedMixAudio {
            reference,
            contributions,
            report: None,
        };
    }

    let residual_norm = candidate
        .iter()
        .zip(reference.iter())
        .map(|(actual, expected)| f64::from(*actual - *expected).powi(2))
        .sum::<f64>()
        .sqrt();
    let residual_ratio = residual_norm / reference_norm;
    let deck_gain_estimates = estimate_deck_gains(candidate, &contributions).unwrap_or_default();
    let candidate_rms = rms(candidate);
    let reference_rms = rms(&reference);
    let rms_delta_db = (candidate_rms > 0.0 && reference_rms > 0.0)
        .then(|| 20.0 * (candidate_rms / reference_rms).log10());
    let report = MatchedMixReport {
        candidate_rms_dbfs: dbfs(candidate_rms),
        reference_rms_dbfs: dbfs(reference_rms),
        rms_delta_db,
        residual_ratio,
        deck_contribution_ratios,
        deck_gain_estimates,
    };
    if report.deck_gain_estimates.len() != contributions.len()
        || report
            .deck_gain_estimates
            .iter()
            .any(|gain| !gain.is_finite() || (*gain - 1.0).abs() > MAX_DECK_GAIN_DELTA)
    {
        failures.push(format!(
            "{label}: final mix deck gains differ from their independent references: estimates={:?}, limit=1.0+/-{MAX_DECK_GAIN_DELTA:.3}, residual={residual_ratio:.6}, deck_contributions={:?}",
            report.deck_gain_estimates, report.deck_contribution_ratios,
        ));
    }
    if rms_delta_db.is_none_or(|delta| delta.abs() > MAX_MATCHED_RMS_DELTA_DB) {
        failures.push(format!(
            "{label}: final mix level differs from its independent reference by {rms_delta_db:?}dB, limit=+/-{MAX_MATCHED_RMS_DELTA_DB:.3}dB",
        ));
    }
    MatchedMixAudio {
        reference,
        contributions,
        report: Some(report),
    }
}

/// `no_block`: one Cochlea pass per stem, mix and reference. Same reason as
/// `assess_audio`, and this one runs once per deck.
#[kithara::allow_block]
pub(super) fn measure_audio_level(
    label: &str,
    role: AudioRole,
    sample_rate: u32,
    samples: &[f32],
) -> AudioLevelReport {
    if samples.is_empty() {
        return AudioLevelReport {
            label: label.to_owned(),
            role,
            rms_dbfs: None,
            peak_dbfs: None,
            integrated_lufs: None,
            momentary_max_lufs: None,
            sample_peak_dbfs: None,
            true_peak_dbtp: None,
        };
    }
    let report = CochleaReport::measure(samples, CHANNELS, sample_rate);
    AudioLevelReport {
        label: label.to_owned(),
        role,
        rms_dbfs: dbfs(rms(samples)),
        peak_dbfs: dbfs(
            samples
                .iter()
                .map(|sample| f64::from(sample.abs()))
                .fold(0.0, f64::max),
        ),
        integrated_lufs: report.integrated_lufs,
        momentary_max_lufs: report.momentary_max_lufs,
        sample_peak_dbfs: report.sample_peak_dbfs,
        true_peak_dbtp: report.true_peak_dbtp,
    }
}

pub(super) fn assess_listening_levels(
    case: &str,
    levels: &[AudioLevelReport],
    failures: &mut Vec<String>,
) {
    for level in levels {
        if level.rms_dbfs.is_none() {
            failures.push(format!("{case} {}: PCM level is undefined", level.label));
        }
        if matches!(level.role, AudioRole::DirectReference)
            && level
                .rms_dbfs
                .is_none_or(|rms| rms < MIN_FIXED_STEM_RMS_DBFS)
        {
            failures.push(format!(
                "{case} {}: fixed 10s fixture window is not audibly load-bearing: rms={:?}dBFS, minimum={MIN_FIXED_STEM_RMS_DBFS:.1}dBFS",
                level.label, level.rms_dbfs,
            ));
        }
        if !matches!(level.role, AudioRole::Contribution) && level.integrated_lufs.is_none() {
            failures.push(format!(
                "{case} {}: Cochlea integrated loudness is undefined",
                level.label,
            ));
        }
    }
}

fn l2_norm(samples: &[f32]) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn l2_norm_scaled(samples: &[f32], scale: f32) -> f64 {
    samples
        .iter()
        .map(|sample| f64::from(*sample * scale).powi(2))
        .sum::<f64>()
        .sqrt()
}

fn estimate_deck_gains(candidate: &[f32], contributions: &[Vec<f32>]) -> Option<Vec<f64>> {
    let count = contributions.len();
    if count == 0
        || contributions
            .iter()
            .any(|values| values.len() != candidate.len())
    {
        return None;
    }

    let mut system = vec![vec![0.0; count + 1]; count];
    for row in 0..count {
        for column in 0..count {
            system[row][column] = contributions[row]
                .iter()
                .zip(&contributions[column])
                .map(|(left, right)| f64::from(*left) * f64::from(*right))
                .sum();
        }
        system[row][count] = contributions[row]
            .iter()
            .zip(candidate)
            .map(|(reference, actual)| f64::from(*reference) * f64::from(*actual))
            .sum();
    }

    for pivot in 0..count {
        let resolved = (pivot..count).max_by(|left, right| {
            system[*left][pivot]
                .abs()
                .total_cmp(&system[*right][pivot].abs())
        })?;
        if system[resolved][pivot].abs() <= f64::EPSILON {
            return None;
        }
        system.swap(pivot, resolved);
        let divisor = system[pivot][pivot];
        for column in pivot..=count {
            system[pivot][column] /= divisor;
        }
        for row in 0..count {
            if row == pivot {
                continue;
            }
            let factor = system[row][pivot];
            for column in pivot..=count {
                system[row][column] -= factor * system[pivot][column];
            }
        }
    }

    Some(system.into_iter().map(|row| row[count]).collect())
}

fn dbfs(amplitude: f64) -> Option<f64> {
    (amplitude.is_finite() && amplitude > 0.0).then(|| 20.0 * amplitude.log10())
}

pub(super) fn blocks_for_secs(sample_rate: u32, seconds: u32) -> usize {
    let frames = u64::from(sample_rate) * u64::from(seconds);
    usize::try_from(frames)
        .expect("matrix duration fits usize")
        .div_ceil(BLOCK_FRAMES)
}

fn measure_sample_continuity(samples: &[f32]) -> SampleContinuityReport {
    let channels = usize::from(CHANNELS);
    let frames = samples.len() / channels;
    let mut longest_zero = 0usize;
    let mut zero_run = 0usize;
    for frame in samples[..frames * channels].chunks_exact(channels) {
        if frame.iter().all(|sample| *sample == 0.0) {
            zero_run += 1;
            longest_zero = longest_zero.max(zero_run);
        } else {
            zero_run = 0;
        }
    }

    let mut adjacent = Vec::with_capacity(frames.saturating_sub(1) * channels);
    for frame in 1..frames {
        for channel in 0..channels {
            let before = samples[(frame - 1) * channels + channel];
            let after = samples[frame * channels + channel];
            adjacent.push((after - before).abs());
        }
    }
    adjacent.sort_by(f32::total_cmp);
    let p99_adjacent_jump = adjacent
        .get(adjacent.len().saturating_sub(1) * 99 / 100)
        .copied()
        .unwrap_or(0.0);
    let boundary_limit = MIN_BOUNDARY_JUMP.max(p99_adjacent_jump * BOUNDARY_OUTLIER_RATIO);

    let mut discontinuities = Vec::new();
    let mut repeated = Vec::new();
    let mut max_boundary_jump = 0.0_f32;
    for boundary in (BLOCK_FRAMES..frames).step_by(BLOCK_FRAMES) {
        let jump = (0..channels)
            .map(|channel| {
                let before = samples[(boundary - 1) * channels + channel];
                let after = samples[boundary * channels + channel];
                (after - before).abs()
            })
            .fold(0.0_f32, f32::max);
        max_boundary_jump = max_boundary_jump.max(jump);
        if jump > boundary_limit {
            discontinuities.push(boundary);
        }

        let previous = (boundary - BLOCK_FRAMES) * channels..boundary * channels;
        let current = boundary * channels..(boundary + BLOCK_FRAMES).min(frames) * channels;
        if current.len() == previous.len() && samples[previous] == samples[current] {
            repeated.push(boundary);
        }
    }

    SampleContinuityReport {
        discontinuity_boundaries: discontinuities,
        longest_exact_zero_run_frames: longest_zero,
        max_boundary_jump,
        p99_adjacent_jump,
        repeated_block_boundaries: repeated,
    }
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let power = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>()
        / f64::from(u32::try_from(samples.len()).expect("callback samples fit u32"));
    power.sqrt()
}

#[cfg(test)]
mod tests {
    use kithara::platform::no_block::force_panic_mode;

    use super::{super::SOURCE_RATE, *};

    /// The Cochlea oracles are seconds of arithmetic. Unsanctioned they are
    /// measured against the poll budget of whatever async test calls them,
    /// which is what failed the matrix on a loaded CI host.
    #[kithara::test(flash(false))]
    async fn cochlea_oracles_are_sanctioned_offline_work() {
        let _mode = force_panic_mode();
        let frames = usize::try_from(SOURCE_RATE).expect("source rate fits usize") / 2;
        let capture = (0..frames * usize::from(CHANNELS))
            .map(|index| (index as f32 * 0.017).sin() * 0.4)
            .collect::<Vec<_>>();
        watched_oracles(&capture).await;
    }

    /// A millisecond is far below any Cochlea pass, so the strict tier answers
    /// an unsanctioned call whatever the host's CPU-to-wall ratio says.
    #[kithara::no_block(budget_ms = 1)]
    async fn watched_oracles(capture: &[f32]) {
        let mut failures = Vec::new();
        let _ = assess_audio("no-block-guard", SOURCE_RATE, capture, &mut failures);
        let _ = measure_audio_level("no-block-guard", AudioRole::FinalMix, SOURCE_RATE, capture);
    }

    #[kithara::test(native, flash(false))]
    fn matched_mix_rejects_global_attenuation_and_each_missing_deck() {
        let samples = 8_192;
        let stem_a = (0..samples)
            .map(|index| (index as f32 * 0.017).sin() * 0.4)
            .collect::<Vec<_>>();
        let stem_b = (0..samples)
            .map(|index| (index as f32 * 0.031).cos() * 0.3)
            .collect::<Vec<_>>();
        let stems = [stem_a.as_slice(), stem_b.as_slice()];
        let scale = MIX_HEADROOM / 2.0;
        let reference = stem_a
            .iter()
            .zip(stem_b.iter())
            .map(|(a, b)| (*a + *b) * scale)
            .collect::<Vec<_>>();

        let mut failures = Vec::new();
        let matched = assess_matched_mix("exact", &reference, &stems, &mut failures);
        assert!(failures.is_empty(), "exact mix failed: {failures:?}");
        assert_eq!(matched.report.map(|value| value.residual_ratio), Some(0.0));

        let quiet = reference
            .iter()
            .map(|sample| *sample * 0.25)
            .collect::<Vec<_>>();
        let mut quiet_failures = Vec::new();
        let _ = assess_matched_mix("quiet", &quiet, &stems, &mut quiet_failures);
        assert!(
            !quiet_failures.is_empty(),
            "matched oracle accepted global attenuation"
        );

        let mut dropout = reference.clone();
        let channels = usize::from(CHANNELS);
        let midpoint_frame = dropout.len() / channels / 2;
        let dropout_start = midpoint_frame * channels;
        let dropout_samples = BLOCK_FRAMES * channels;
        dropout[dropout_start..dropout_start + dropout_samples].fill(0.0);
        let mut dropout_failures = Vec::new();
        let _ = assess_audio("dropout", 44_100, &dropout, &mut dropout_failures);
        assert!(
            !dropout_failures.is_empty(),
            "matched oracle accepted a zeroed callback quantum"
        );

        for (attenuated, stem) in [("a", &stem_a), ("b", &stem_b)] {
            let candidate = reference
                .iter()
                .zip(stem.iter())
                .map(|(full, source)| *full - *source * scale * 0.1)
                .collect::<Vec<_>>();
            let mut attenuation_failures = Vec::new();
            let _ = assess_matched_mix(attenuated, &candidate, &stems, &mut attenuation_failures);
            assert!(
                !attenuation_failures.is_empty(),
                "matched oracle accepted 10% attenuation of deck {attenuated}"
            );
        }

        for (missing, candidate) in [
            (
                "a",
                stem_b
                    .iter()
                    .map(|sample| *sample * scale)
                    .collect::<Vec<f32>>(),
            ),
            (
                "b",
                stem_a
                    .iter()
                    .map(|sample| *sample * scale)
                    .collect::<Vec<f32>>(),
            ),
        ] {
            let mut missing_failures = Vec::new();
            let _ = assess_matched_mix(missing, &candidate, &stems, &mut missing_failures);
            assert!(
                !missing_failures.is_empty(),
                "matched oracle accepted missing deck {missing}"
            );
        }
    }
}
