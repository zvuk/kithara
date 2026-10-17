use kithara::warp::StretchKind;
#[cfg(not(target_os = "android"))]
use kithara_integration_tests::audio_artifact::write_audio_artifact;
use kithara_integration_tests::cochlea::{
    CochleaReport, assert_oracle_load_bearing, time_stretch_failures,
};
use kithara_test_fixtures::assets::{marked_sine_wav_a440_6s, sine_wav_a440_6s};
#[cfg(not(target_os = "android"))]
use serde::Serialize;

#[cfg(not(target_os = "android"))]
use super::mock::{CaptureMetrics, ChannelContinuity, MarkerTiming, SineFit};
use super::mock::{
    assert_frame_oracle_load_bearing, first_channel, first_sample_mismatch, frame_continuity,
    marker_timing, marker_timing_failures, measure_quiet_sine, render_passthrough, tone_dominates,
};
use crate::consts::{
    ACTIVE_SPEED, BLOCK_FRAMES, CHANNELS, MAX_ACTIVE_STEREO_DELTA, PITCH_SHIFTED_TONE_HZ,
    SAMPLE_RATE, TONE_HZ,
};

#[cfg(not(target_os = "android"))]
#[derive(Serialize)]
struct ActiveStretchManifest<'a> {
    case: &'static str,
    backend: &'a str,
    speed: f32,
    sample_rate: u32,
    channels: u16,
    control: CaptureMetrics,
    candidate: CaptureMetrics,
    control_cochlea: &'a CochleaReport,
    candidate_cochlea: &'a CochleaReport,
    candidate_sine_fit: &'a SineFit,
    candidate_continuity: &'a [ChannelContinuity],
    rate_control: &'a MarkerTiming,
    rate_candidate: &'a MarkerTiming,
    failures: &'a [String],
}

/// Renders the tone through an active stretch and checks its rate and pitch.
///
/// # Panics
///
/// Panics when a fixture differs from the given bytes or the stretch misses its targets.
pub async fn run_active_stretch(
    source: &'static [u8],
    marker_source: &'static [u8],
    shifted_pitch: Vec<f32>,
    backend: StretchKind,
    record_artifacts: bool,
) {
    let asset = sine_wav_a440_6s();
    let marker_asset = marked_sine_wav_a440_6s();
    assert_eq!(asset.bytes(), source);
    assert_eq!(marker_asset.bytes(), marker_source);
    let control = render_passthrough(&asset, None, false).await;
    let candidate = render_passthrough(&asset, Some((backend, ACTIVE_SPEED)), false).await;
    let rate_control = render_passthrough(&marker_asset, None, false).await;
    let rate_candidate =
        render_passthrough(&marker_asset, Some((backend, ACTIVE_SPEED)), false).await;
    let control_report = CochleaReport::measure(&control.pcm, CHANNELS, SAMPLE_RATE);
    let candidate_report = CochleaReport::measure(&candidate.pcm, CHANNELS, SAMPLE_RATE);
    let candidate_sine_fit = measure_quiet_sine(&candidate.pcm);
    let (candidate_continuity, continuity_failures) = frame_continuity(&candidate.pcm);
    let mut failures = time_stretch_failures("active keylock", &candidate_report, &control_report);
    failures.extend(continuity_failures);
    let rate_control_timing = marker_timing(&rate_control.pcm, 1.0);
    let rate_candidate_timing = marker_timing(&rate_candidate.pcm, ACTIVE_SPEED);
    failures.extend(marker_timing_failures(
        "unity rate control",
        &rate_control_timing,
    ));
    failures.extend(marker_timing_failures(
        "active keylock rate",
        &rate_candidate_timing,
    ));
    let unchanged_rate_negative = marker_timing(&rate_control.pcm, ACTIVE_SPEED);
    assert!(
        !marker_timing_failures("unchanged-rate negative control", &unchanged_rate_negative)
            .is_empty(),
        "rate oracle accepted unity playback as {ACTIVE_SPEED}x"
    );

    for (label, capture) in [
        ("effect-free", &control),
        ("active keylock", &candidate),
        ("rate control", &rate_control),
        ("active rate", &rate_candidate),
    ] {
        let non_finite = capture
            .pcm
            .iter()
            .filter(|sample| !sample.is_finite())
            .count();
        if non_finite != 0 {
            failures.push(format!("{label}: {non_finite} non-finite PCM sample(s)"));
        }
        if capture.warmup_decode_errors != 0 || capture.decode_errors != 0 {
            failures.push(format!(
                "{label}: decode errors warmup={}, capture={}",
                capture.warmup_decode_errors, capture.decode_errors,
            ));
        }
        if capture.warmup_underruns != 0 || capture.underruns != 0 {
            failures.push(format!(
                "{label}: underruns warmup={}, capture={}",
                capture.warmup_underruns, capture.underruns,
            ));
        }
    }

    if first_sample_mismatch(&candidate.pcm, &control.pcm).is_none() {
        failures.push("active keylock remained on the unity passthrough path".to_owned());
    }
    if candidate_sine_fit.non_finite_samples != 0
        || candidate_sine_fit.max_stereo_delta > MAX_ACTIVE_STEREO_DELTA
        || !(0.47..=0.51).contains(&candidate_sine_fit.amplitude)
        || candidate_sine_fit.dc.abs() > 0.002
        || candidate_sine_fit.residual_ratio > 0.01
    {
        failures.push(format!(
            "active keylock violates the independent 440 Hz signal oracle: {candidate_sine_fit:?}",
        ));
    }
    let candidate_mono = first_channel(&candidate.pcm);
    if !tone_dominates(&candidate_mono, TONE_HZ, PITCH_SHIFTED_TONE_HZ) {
        failures.push(format!(
            "active keylock did not preserve {TONE_HZ} Hz over the pitch-shifted {PITCH_SHIFTED_TONE_HZ} Hz control",
        ));
    }
    let wrong_pitch = &shifted_pitch[..candidate_mono.len()];
    assert!(
        tone_dominates(wrong_pitch, PITCH_SHIFTED_TONE_HZ, TONE_HZ),
        "wrong-pitch negative control does not contain its declared frequency"
    );
    assert!(
        !tone_dominates(wrong_pitch, TONE_HZ, PITCH_SHIFTED_TONE_HZ),
        "pitch oracle accepted a deliberately wrong 352 Hz fixture as 440 Hz"
    );

    assert_oracle_load_bearing(&control.pcm, CHANNELS, SAMPLE_RATE, BLOCK_FRAMES);
    assert_oracle_load_bearing(&candidate.pcm, CHANNELS, SAMPLE_RATE, BLOCK_FRAMES);
    assert_frame_oracle_load_bearing(&candidate.pcm);
    #[cfg(target_os = "android")]
    assert!(
        !record_artifacts,
        "listening artifact export belongs to the host suite"
    );
    #[cfg(not(target_os = "android"))]
    if record_artifacts {
        let backend_label = backend.to_string().to_ascii_lowercase();
        let artifact_case = format!("no-sync-active-keylock-{backend_label}");
        let manifest = ActiveStretchManifest {
            case: "no-sync-active-keylock",
            backend: &backend_label,
            speed: ACTIVE_SPEED,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
            control: CaptureMetrics::from(&control),
            candidate: CaptureMetrics::from(&candidate),
            control_cochlea: &control_report,
            candidate_cochlea: &candidate_report,
            candidate_sine_fit: &candidate_sine_fit,
            candidate_continuity: &candidate_continuity,
            rate_control: &rate_control_timing,
            rate_candidate: &rate_candidate_timing,
            failures: &failures,
        };
        let written = write_audio_artifact(
            &artifact_case,
            SAMPLE_RATE,
            CHANNELS,
            &[
                ("effect-free-control", &control.pcm),
                ("active", &candidate.pcm),
                ("rate-marker-control", &rate_control.pcm),
                ("rate-marker-active", &rate_candidate.pcm),
            ],
            &manifest,
        )
        .expect("active no-SYNC audio artifact write");
        assert!(
            written.is_some(),
            "KITHARA_AUDIO_ARTIFACT_DIR must be set for the artifact recorder"
        );
    }
    assert!(
        failures.is_empty(),
        "active no-SYNC stretch failed for {backend}: {}\ncontrol={control_report:?}\ncandidate={candidate_report:?}\ncontinuity={candidate_continuity:?}",
        failures.join("; "),
    );
}
