use kithara::{decode::DecoderBackend, platform::time::Duration};
use kithara_integration_tests::{TestServerHelper, fixture_protocol::PackagedSignal};
use num_traits::ToPrimitive;

use super::{
    AAC_HIGH, AAC_LOW, BLOCK_FRAMES, CHANNELS, FLAC, Transition, control_lag_frames,
    fixture_with_signal, observation_frames, render_no_switch_control, render_switch,
};

const SWEEP_START_HZ: f64 = 80.0;
const SWEEP_END_HZ: f64 = 8_000.0;
const MAX_LAG_FRAMES: isize = 2_048;
const MAX_ACCEPTED_LAG_FRAMES: isize = 4;
const CORRELATION_FRAMES: usize = 8_192;
const POST_BOUNDARY_GUARD_FRAMES: usize = 44_100 + BLOCK_FRAMES * 8;
const MIN_CORRELATION: f64 = 0.85;
const CONTROL_CORRELATION_SLACK: f64 = 0.01;

/// Every ordered pair of the fixture's variants — see `TRANSITIONS`.
const PROVENANCE_TRANSITIONS: [Transition; 6] = [
    Transition::new("aac-low-to-aac-high-sweep", AAC_LOW, AAC_HIGH),
    Transition::new("aac-high-to-flac-sweep", AAC_HIGH, FLAC),
    Transition::new("flac-to-aac-high-sweep", FLAC, AAC_HIGH),
    Transition::new("aac-high-to-aac-low-sweep", AAC_HIGH, AAC_LOW),
    Transition::new("aac-low-to-flac-sweep", AAC_LOW, FLAC),
    Transition::new("flac-to-aac-low-sweep", FLAC, AAC_LOW),
];

#[derive(Clone, Copy, Debug)]
struct LagCorrelation {
    lag_frames: isize,
    correlation: f64,
}

fn best_lag_correlation(switched: &[f32], source: &[f32], start_frame: usize) -> LagCorrelation {
    let frames = switched.len() / usize::from(CHANNELS);
    assert_eq!(
        source.len(),
        switched.len(),
        "switch and source control renders must have equal lengths",
    );
    let earliest_source_frame = start_frame
        .checked_add_signed(-MAX_LAG_FRAMES)
        .expect("post-boundary guard must cover the negative lag range");
    let latest_source_end = start_frame
        .checked_add_signed(MAX_LAG_FRAMES)
        .and_then(|frame| frame.checked_add(CORRELATION_FRAMES))
        .expect("correlation window bounds must fit usize");
    assert!(
        earliest_source_frame < frames && latest_source_end <= frames,
        "correlation window must fit the fixed observation: start={start_frame}, earliest={earliest_source_frame}, latest_end={latest_source_end}, frames={frames}",
    );

    let channels = usize::from(CHANNELS);
    let switched_channel = switched
        .chunks_exact(channels)
        .map(|frame| f64::from(frame[0]))
        .collect::<Vec<_>>();
    let source_channel = source
        .chunks_exact(channels)
        .map(|frame| f64::from(frame[0]))
        .collect::<Vec<_>>();
    let frame_count = CORRELATION_FRAMES
        .to_f64()
        .expect("correlation frame count fits f64");
    let switched_window = &switched_channel[start_frame..start_frame + CORRELATION_FRAMES];
    let switched_mean = switched_window.iter().sum::<f64>() / frame_count;
    let switched_centered = switched_window
        .iter()
        .map(|sample| sample - switched_mean)
        .collect::<Vec<_>>();
    let switched_centered_sum = switched_centered.iter().sum::<f64>();
    let switched_energy = switched_centered
        .iter()
        .map(|sample| sample * sample)
        .sum::<f64>();

    let mut source_sums = Vec::with_capacity(source_channel.len().saturating_add(1));
    let mut source_square_sums = Vec::with_capacity(source_channel.len().saturating_add(1));
    source_sums.push(0.0);
    source_square_sums.push(0.0);
    for sample in &source_channel {
        source_sums.push(source_sums.last().copied().unwrap_or_default() + sample);
        source_square_sums
            .push(source_square_sums.last().copied().unwrap_or_default() + sample * sample);
    }

    (-MAX_LAG_FRAMES..=MAX_LAG_FRAMES)
        .map(|lag_frames| {
            let source_start = start_frame
                .checked_add_signed(lag_frames)
                .expect("validated lag keeps the source window in bounds");
            let source_end = source_start.saturating_add(CORRELATION_FRAMES);
            let source_sum = source_sums[source_end] - source_sums[source_start];
            let source_square_sum =
                source_square_sums[source_end] - source_square_sums[source_start];
            let source_mean = source_sum / frame_count;
            let source_energy = (source_square_sum - source_sum * source_mean).max(0.0);
            let covariance = switched_centered
                .iter()
                .zip(&source_channel[source_start..source_end])
                .fold(0.0, |sum, (switched, source)| {
                    switched.mul_add(*source, sum)
                })
                - source_mean * switched_centered_sum;
            LagCorrelation {
                lag_frames,
                correlation: covariance / (switched_energy * source_energy).sqrt(),
            }
        })
        .max_by(|left, right| left.correlation.total_cmp(&right.correlation))
        .expect("non-empty lag search")
}

#[kithara::test(
    tokio,
    multi_thread,
    native,
    serial,
    timeout(Duration::from_secs(180)),
    hang_timeout_secs(3)
)]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn decoder_recreation_preserves_sweep_timeline_against_no_switch_control(
    #[case] backend: DecoderBackend,
) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(fixture_with_signal(PackagedSignal::Sweep {
            start_hz: SWEEP_START_HZ,
            end_hz: SWEEP_END_HZ,
        }))
        .await
        .expect("create nonperiodic quality-switch fixture");
    let master_url = created.master_url();

    let controls = [
        render_no_switch_control(
            &master_url,
            AAC_LOW,
            backend,
            "aac-low-sweep-no-switch-control",
        )
        .await,
        render_no_switch_control(
            &master_url,
            AAC_HIGH,
            backend,
            "aac-high-sweep-no-switch-control",
        )
        .await,
        render_no_switch_control(&master_url, FLAC, backend, "flac-sweep-no-switch-control").await,
    ];
    for (variant, control) in controls.iter().enumerate() {
        assert_eq!(
            control.capture_frame, controls[0].capture_frame,
            "sweep control variant {variant} must capture the exact same source frame",
        );
        assert_eq!(
            control.samples.len() / usize::from(CHANNELS),
            observation_frames(),
            "sweep control variant {variant} fixed observation length",
        );
    }

    let mut failures = Vec::new();
    for transition in PROVENANCE_TRANSITIONS {
        let switched = render_switch(&master_url, transition, backend).await;
        let source_control = &controls[transition.from];
        let destination_control = &controls[transition.to];
        assert_eq!(
            switched.capture_frame, source_control.capture_frame,
            "{} must use the exact same source fragment as its no-switch control",
            transition.label,
        );
        let correlation_start = switched
            .target_decoder_frame
            .saturating_add(POST_BOUNDARY_GUARD_FRAMES);
        let best = best_lag_correlation(
            &switched.samples,
            &source_control.samples,
            correlation_start,
        );
        let control_baseline = best_lag_correlation(
            &destination_control.samples,
            &source_control.samples,
            correlation_start,
        );
        let expected_control_lag = control_lag_frames(transition);
        let required_correlation =
            (control_baseline.correlation - CONTROL_CORRELATION_SLACK).max(MIN_CORRELATION);
        println!(
            "QUALITY_SWITCH_TIMELINE transition={} backend={backend:?} capture_source_frame={} target_decoder_frame={} correlation_start={} lag_frames={} correlation={:.9} control_lag_frames={} expected_control_lag_frames={} control_correlation={:.9} required_correlation={:.9}",
            transition.label,
            switched.capture_frame,
            switched.target_decoder_frame,
            correlation_start,
            best.lag_frames,
            best.correlation,
            control_baseline.lag_frames,
            expected_control_lag,
            control_baseline.correlation,
            required_correlation,
        );
        if best.correlation < required_correlation {
            failures.push(format!(
                "{} post-switch PCM no longer matches its no-switch source control: lag={} frames, correlation={:.9}, codec-control={:.9}, required={required_correlation:.9}",
                transition.label,
                best.lag_frames,
                best.correlation,
                control_baseline.correlation,
            ));
        }
        if best.lag_frames.abs() > MAX_ACCEPTED_LAG_FRAMES {
            failures.push(format!(
                "{} shifted the musical timeline relative to the same no-switch fragment: lag={} frames, correlation={:.9}, allowed=+/-{MAX_ACCEPTED_LAG_FRAMES}",
                transition.label, best.lag_frames, best.correlation,
            ));
        }
        if (control_baseline.lag_frames - expected_control_lag).abs() > MAX_ACCEPTED_LAG_FRAMES {
            failures.push(format!(
                "{} source and destination no-switch controls disagree with their codec origins: lag={} frames, expected={} frames, correlation={:.9}, allowed=+/-{MAX_ACCEPTED_LAG_FRAMES}",
                transition.label,
                control_baseline.lag_frames,
                expected_control_lag,
                control_baseline.correlation,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "decoder recreation changed sweep provenance for backend {backend:?}:\n{}",
        failures.join("\n"),
    );
}
