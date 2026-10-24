use kithara::{
    decode::DecoderBackend,
    platform::{time::Duration, tokio::join},
};
use kithara_integration_tests::{CreatedHls, TestServerHelper, fixture_protocol::PackagedSignal};
use num_traits::ToPrimitive;
use rustfft::{FftPlanner, num_complex::Complex};

use super::{
    AAC_HIGH, AAC_LOW, BLOCK_FRAMES, CHANNELS, FLAC, Transition, fixture_with_signal,
    observation_frames, render_no_switch_control, render_switch,
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

fn sliding_dot_products(window: &[f64], source: &[f64]) -> (Vec<f64>, f64) {
    let fft_len = (window.len() + source.len() - 1).next_power_of_two();
    let mut switched_spectrum = vec![Complex::default(); fft_len];
    let mut source_spectrum = vec![Complex::default(); fft_len];
    for (bin, sample) in switched_spectrum.iter_mut().zip(window.iter().rev()) {
        bin.re = *sample;
    }
    for (bin, sample) in source_spectrum.iter_mut().zip(source) {
        bin.re = *sample;
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_len);
    forward.process(&mut switched_spectrum);
    forward.process(&mut source_spectrum);
    for (switched, source) in switched_spectrum.iter_mut().zip(source_spectrum) {
        *switched *= source;
    }
    planner
        .plan_fft_inverse(fft_len)
        .process(&mut switched_spectrum);
    let normalization = fft_len.to_f64().expect("FFT length fits f64");

    // Bound both FFT roundoff and the original sequential dot product before
    // selecting contenders. Exact dots settle every numerically ambiguous peak.
    let gamma = |operations: u32| {
        let error = f64::from(operations) * f64::EPSILON;
        error / (1.0 - error)
    };
    let switched_l1 = window.iter().map(|sample| sample.abs()).sum::<f64>();
    let source_l1 = source.iter().map(|sample| sample.abs()).sum::<f64>();
    let source_peak = source.iter().map(|sample| sample.abs()).fold(0.0, f64::max);
    // Allow 128 real rounding operations per stage for the three complex
    // transforms, their twiddles, and the pointwise products.
    let covariance_error = gamma(128 * fft_len.ilog2() + 16) * switched_l1 * source_l1
        + gamma(u32::try_from(window.len()).expect("window fits u32")) * switched_l1 * source_peak;
    let dots = switched_spectrum[window.len() - 1..source.len()]
        .iter()
        .map(|bin| bin.re / normalization)
        .collect();
    (dots, covariance_error)
}

fn source_prefix_sums(source: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut source_sums = Vec::with_capacity(source.len().saturating_add(1));
    let mut source_square_sums = Vec::with_capacity(source.len().saturating_add(1));
    source_sums.push(0.0);
    source_square_sums.push(0.0);
    for sample in source {
        source_sums.push(source_sums.last().copied().unwrap_or_default() + sample);
        source_square_sums
            .push(source_square_sums.last().copied().unwrap_or_default() + sample * sample);
    }

    (source_sums, source_square_sums)
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

    let (source_sums, source_square_sums) = source_prefix_sums(&source_channel);

    let source_span = &source_channel[earliest_source_frame..latest_source_end];
    let (dots, covariance_error) = sliding_dot_products(&switched_centered, source_span);
    let candidates = (-MAX_LAG_FRAMES..=MAX_LAG_FRAMES)
        .enumerate()
        .map(|(offset, lag_frames)| {
            let source_start = start_frame
                .checked_add_signed(lag_frames)
                .expect("validated lag keeps the source window in bounds");
            let source_end = source_start.saturating_add(CORRELATION_FRAMES);
            let source_sum = source_sums[source_end] - source_sums[source_start];
            let source_square_sum =
                source_square_sums[source_end] - source_square_sums[source_start];
            let source_mean = source_sum / frame_count;
            let source_energy = (source_square_sum - source_sum * source_mean).max(0.0);
            let covariance = dots[offset] - source_mean * switched_centered_sum;
            let denominator = (switched_energy * source_energy).sqrt();
            (
                LagCorrelation {
                    lag_frames,
                    correlation: covariance / denominator,
                },
                covariance_error / denominator,
            )
        })
        .collect::<Vec<_>>();
    let lower_bound = candidates
        .iter()
        .filter(|(candidate, error)| candidate.correlation.is_finite() && error.is_finite())
        .map(|(candidate, error)| (candidate.correlation - error).next_down())
        .fold(f64::NEG_INFINITY, f64::max);
    candidates
        .into_iter()
        .filter(|(candidate, error)| {
            !candidate.correlation.is_finite()
                || !error.is_finite()
                || (candidate.correlation + error).next_up() >= lower_bound
        })
        .map(|(candidate, _)| {
            let source_start = start_frame
                .checked_add_signed(candidate.lag_frames)
                .expect("validated lag keeps the source window in bounds");
            let source_end = source_start + CORRELATION_FRAMES;
            let source_sum = source_sums[source_end] - source_sums[source_start];
            let source_mean = source_sum / frame_count;
            let source_square_sum =
                source_square_sums[source_end] - source_square_sums[source_start];
            let source_energy = (source_square_sum - source_sum * source_mean).max(0.0);
            let covariance = switched_centered
                .iter()
                .zip(&source_channel[source_start..source_end])
                .fold(0.0, |sum, (switched, source)| {
                    switched.mul_add(*source, sum)
                })
                - source_mean * switched_centered_sum;
            LagCorrelation {
                lag_frames: candidate.lag_frames,
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
    timeout(Duration::from_secs(20)),
    hang_timeout_secs(3)
)]
#[cfg_attr(not(target_os = "android"), case::symphonia(DecoderBackend::Symphonia))]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::default()))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn decoder_recreation_preserves_sweep_timeline_against_no_switch_control(
    #[case] backend: DecoderBackend,
    #[future(awt)] sweep_source: (TestServerHelper, CreatedHls),
) {
    let (_server, created) = sweep_source;
    let master_url = created.master_url();

    // The references wait on decode rather than on a clock, so they render
    // side by side; each switch plays in real time beside one other.
    let (aac_low, aac_high, flac) = join!(
        render_no_switch_control(
            &master_url,
            AAC_LOW,
            backend,
            "aac-low-sweep-no-switch-control",
        ),
        render_no_switch_control(
            &master_url,
            AAC_HIGH,
            backend,
            "aac-high-sweep-no-switch-control",
        ),
        render_no_switch_control(&master_url, FLAC, backend, "flac-sweep-no-switch-control"),
    );
    let controls = [aac_low, aac_high, flac];
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
    for &[first, second] in PROVENANCE_TRANSITIONS.as_chunks::<2>().0 {
        let (first_switched, second_switched) = join!(
            render_switch(&master_url, first, backend),
            render_switch(&master_url, second, backend),
        );
        for (transition, switched) in [(first, first_switched), (second, second_switched)] {
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
            let required_correlation =
                (control_baseline.correlation - CONTROL_CORRELATION_SLACK).max(MIN_CORRELATION);
            println!(
                "QUALITY_SWITCH_TIMELINE transition={} backend={backend:?} capture_source_frame={} target_decoder_frame={} correlation_start={} lag_frames={} correlation={:.9} control_lag_frames={} control_correlation={:.9} required_correlation={:.9}",
                transition.label,
                switched.capture_frame,
                switched.target_decoder_frame,
                correlation_start,
                best.lag_frames,
                best.correlation,
                control_baseline.lag_frames,
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
            if control_baseline.lag_frames.abs() > MAX_ACCEPTED_LAG_FRAMES {
                failures.push(format!(
                "{} source and destination no-switch controls captured at one position hold different content: lag={} frames, correlation={:.9}, allowed=+/-{MAX_ACCEPTED_LAG_FRAMES}",
                transition.label,
                control_baseline.lag_frames,
                control_baseline.correlation,
            ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "decoder recreation changed sweep provenance for backend {backend:?}:\n{}",
        failures.join("\n"),
    );
}

#[kithara::fixture]
async fn sweep_source() -> (TestServerHelper, CreatedHls) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(fixture_with_signal(PackagedSignal::Sweep {
            start_hz: SWEEP_START_HZ,
            end_hz: SWEEP_END_HZ,
        }))
        .await
        .expect("create nonperiodic quality-switch fixture");
    (server, created)
}

#[kithara::test(native)]
fn fft_correlation_numerical_equivalence() {
    let frames = 16_384usize;
    let channels = usize::from(CHANNELS);
    let start = 4_096usize;
    for shift in [-2_048isize, -5, -4, 0, 4, 5, 2_048] {
        let signal = (0..frames)
            .map(|i| {
                let t = i.to_f64().expect("index fits f64");
                (t * 0.17 + t * t * 0.000_013)
                    .sin()
                    .to_f32()
                    .expect("signal sample fits f32")
            })
            .collect::<Vec<_>>();
        let source = signal
            .iter()
            .flat_map(|sample| std::iter::repeat_n(*sample, channels))
            .collect::<Vec<_>>();
        let switched = (0..frames)
            .flat_map(|i| {
                let sample = i
                    .checked_add_signed(shift)
                    .and_then(|j| signal.get(j))
                    .copied()
                    .unwrap_or_default();
                std::iter::repeat_n(sample, channels)
            })
            .collect::<Vec<_>>();
        let best = best_lag_correlation(&switched, &source, start);
        assert_eq!(best.lag_frames, shift);
        assert_eq!(
            best.lag_frames.abs() > MAX_ACCEPTED_LAG_FRAMES,
            shift.abs() > MAX_ACCEPTED_LAG_FRAMES
        );
    }
    let periodic = (0..frames)
        .flat_map(|i| std::iter::repeat_n(if i % 2 == 0 { 1.0 } else { -1.0 }, channels))
        .collect::<Vec<_>>();
    let tied = best_lag_correlation(&periodic, &periodic, start);
    assert_eq!(tied.lag_frames, MAX_LAG_FRAMES);
    let noise = (0..frames)
        .flat_map(|i| {
            std::iter::repeat_n(
                ((i * 7919 % 65521).to_f32().expect("noise sample fits f32") / 32760.5) - 1.0,
                channels,
            )
        })
        .collect::<Vec<_>>();
    let unrelated = best_lag_correlation(&noise, &periodic, start);
    assert!(unrelated.correlation < MIN_CORRELATION);
    let perturbed = periodic
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let frame = (i / channels).to_f32().expect("frame fits f32");
            *sample + 1e-3 * (frame * 0.17).sin()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        best_lag_correlation(&perturbed, &perturbed, start).lag_frames,
        0
    );
    for value in [0.0, 1.0] {
        let samples = vec![value; frames * channels];
        let best = best_lag_correlation(&samples, &samples, start);
        assert!(best.correlation.is_nan());
        assert_eq!(best.lag_frames, MAX_LAG_FRAMES);
    }
}
