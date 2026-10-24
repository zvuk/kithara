#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use cochlea_features::{Audio, analyze_clipping, analyze_loudness};
use kithara::platform::time::Duration;
use kithara_integration_tests::{cochlea::mix_loudness_failures, grid::Start, kithara};
use num_traits::AsPrimitive;

use super::sync_product_matrix::{
    Audible, BLOCK_FRAMES, CHANNELS, NEWTECHNO_PHRASE, PreparedSources, ProductHarness,
    REAL_TRACK_FOUR_DECK_SYNC, REAL_TRACK_SYNC, SyncCase, newtechno_sources, tunnel_sources,
};

const CAPTURE_FRAMES: usize = 48_000 * 6;
const LOUDNESS_TOLERANCE_LU: f64 = 0.5;
const RIDE_STEPS: usize = 32;
/// An entry on the second beat of the first bar.
const WEAK_BEAT: Start = Start::Bar { bar: 0, beat: 1 };

struct Capture {
    pcm: Vec<f32>,
    failures: Vec<String>,
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_solo(
    case: SyncCase,
    provider: &PreparedSources,
    start: Start,
    audible_deck: usize,
) -> Capture {
    hang_tick!("render_solo: waiting for progress".to_owned());
    let mut harness = ProductHarness::new(case, provider, start, Audible::Deck(audible_deck)).await;
    hang_reset!();
    harness.seek_staggered(case).await;
    hang_reset!();
    let pcm = render_frames(&mut harness, case, CAPTURE_FRAMES).await;
    hang_reset!();
    Capture {
        pcm,
        failures: harness.failures,
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn render_mix(
    case: SyncCase,
    provider: &PreparedSources,
    start: Start,
    target_bpm: Option<f64>,
) -> Capture {
    hang_tick!("render_mix: waiting for progress".to_owned());
    let mut harness = ProductHarness::new(case, provider, start, Audible::Mix).await;
    hang_reset!();
    harness.seek_staggered(case).await;
    hang_reset!();
    harness.request_sync(case).await;
    hang_reset!();

    let pcm = if let Some(target_bpm) = target_bpm {
        let mut pcm = Vec::with_capacity(CAPTURE_FRAMES * usize::from(CHANNELS));
        let mut rendered = 0;
        for step in 1..=RIDE_STEPS {
            hang_tick!("render_mix: waiting for progress".to_owned());
            let (step_f, steps): (f64, f64) = (step.as_(), RIDE_STEPS.as_());
            let progress = step_f / steps;
            harness
                .set_tempo(case, (target_bpm - 120.0).mul_add(progress, 120.0), false)
                .await;
            let deadline = CAPTURE_FRAMES * step / RIDE_STEPS;
            pcm.extend(render_frames(&mut harness, case, deadline - rendered).await);
            rendered = deadline;
            hang_reset!();
        }
        pcm
    } else {
        render_frames(&mut harness, case, CAPTURE_FRAMES).await
    };
    Capture {
        pcm,
        failures: harness.failures,
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
pub(super) async fn render_frames(
    harness: &mut ProductHarness,
    case: SyncCase,
    frames: usize,
) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(frames * usize::from(CHANNELS));
    let mut rendered = 0;
    while rendered < frames {
        hang_tick!("render_frames: waiting for progress".to_owned());
        let block_frames = (frames - rendered).min(BLOCK_FRAMES);
        let block = harness.render(case, block_frames).await;
        assert_eq!(
            block.len(),
            block_frames * usize::from(CHANNELS),
            "offline renderer must return the requested complete block",
        );
        pcm.extend_from_slice(&block);
        rendered += block_frames;
        hang_reset!();
    }
    pcm
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::tunnel_weak_beat(tunnel_sources().await, WEAK_BEAT)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn sync_listening_mix_is_not_quieter_than_a_solo_deck(
    #[case] provider: PreparedSources,
    #[case] start: Start,
) {
    let case = REAL_TRACK_SYNC;
    let mut decks = Vec::with_capacity(case.decks());
    for deck in 0..case.decks() {
        hang_tick!(
            "sync_listening_mix_is_not_quieter_than_a_solo_deck: waiting for progress".to_owned()
        );
        let capture = render_solo(case, &provider, start, deck).await;
        decks.push(analyze_loudness(&Audio {
            samples: capture.pcm,
            channels: CHANNELS,
            sample_rate: case.sample_rate,
        }));
        hang_reset!();
    }
    let mix = render_mix(case, &provider, start, None).await;
    let audio = Audio {
        samples: mix.pcm,
        channels: CHANNELS,
        sample_rate: case.sample_rate,
    };
    let mix = analyze_loudness(&audio);
    let clipping = analyze_clipping(&audio, mix.true_peak_dbtp);
    let mut failures = mix_loudness_failures(case.id(), &mix, &decks, LOUDNESS_TOLERANCE_LU);
    if clipping.clipped_samples > 0 || clipping.true_peak_over_0dbtp {
        failures.push(format!("{}: mix clips: {clipping:?}", case.id()));
    }
    assert!(
        failures.is_empty(),
        "sync listening loudness failed:\n{}\ndecks={decks:?}\nmix={mix:?}",
        failures.join("\n"),
    );
}

#[kithara::test(native, tokio, multi_thread, serial, timeout(Duration::from_secs(300)))]
#[case::tunnel_tempo_targets(tunnel_sources().await, Start::bar(0), REAL_TRACK_SYNC, &[None, Some(96.0), Some(127.0), Some(145.0)])]
#[case::tunnel_four_deck_host_120(tunnel_sources().await, Start::bar(0), REAL_TRACK_FOUR_DECK_SYNC, &[None])]
#[case::newtechno_tempo_targets(newtechno_sources().await, NEWTECHNO_PHRASE, REAL_TRACK_SYNC, &[None, Some(96.0), Some(127.0), Some(145.0)])]
#[case::newtechno_four_deck_host_120(newtechno_sources().await, NEWTECHNO_PHRASE, REAL_TRACK_FOUR_DECK_SYNC, &[None])]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(5), ctx = String)]
async fn record_sync_listening_wavs(
    #[case] provider: PreparedSources,
    #[case] start: Start,
    #[case] case: SyncCase,
    #[case] targets: &[Option<f64>],
) {
    let mut deck_reports = Vec::with_capacity(case.decks());
    let mut failures = Vec::new();
    for deck in 0..case.decks() {
        hang_tick!("record_sync_listening_wavs: waiting for progress".to_owned());
        let capture = render_solo(case, &provider, start, deck).await;
        deck_reports.push(analyze_loudness(&Audio {
            samples: capture.pcm,
            channels: CHANNELS,
            sample_rate: case.sample_rate,
        }));
        failures.extend(capture.failures);
        hang_reset!();
    }
    for &target_bpm in targets {
        let mix = render_mix(case, &provider, start, target_bpm).await;
        let audio = Audio {
            samples: mix.pcm,
            channels: CHANNELS,
            sample_rate: case.sample_rate,
        };
        let mix_report = analyze_loudness(&audio);
        let clipping = analyze_clipping(&audio, mix_report.true_peak_dbtp);
        failures.extend(mix.failures);
        failures.extend(mix_loudness_failures(
            case.id(),
            &mix_report,
            &deck_reports,
            LOUDNESS_TOLERANCE_LU,
        ));
        if clipping.clipped_samples > 0 || clipping.true_peak_over_0dbtp {
            failures.push(format!("{}: mix clips: {clipping:?}", case.id()));
        }
        assert!(
            failures.is_empty(),
            "{} listening capture target={target_bpm:?} failed:\n{}\ndecks={deck_reports:?}\nmix={mix_report:?}",
            case.id(),
            failures.join("\n"),
        );
    }
}
