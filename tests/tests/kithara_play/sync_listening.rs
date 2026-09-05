#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use kithara::platform::time::Duration;
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactSet, audio_artifact_path},
    cochlea::{CochleaReport, mix_loudness_failures},
    kithara,
};

use super::sync_product_matrix::{
    AMBIENT_TRIP_HOP_PROVIDER, AMBIENT_TRIP_HOP_SYNC, BLOCK_FRAMES, CHANNELS, CROSS_STYLE_PROVIDER,
    CROSS_STYLE_SYNC, DOWNTEMPO_HOUSE_PROVIDER, DOWNTEMPO_HOUSE_SYNC, ProductHarness, Provider,
    SEQUENTIAL_SYNC, SyncCase, TECHNO_BREAKBEAT_PROVIDER, TECHNO_BREAKBEAT_SYNC,
};

const CAPTURE_FRAMES: usize = 48_000 * 6;
const LOUDNESS_TOLERANCE_LU: f64 = 0.5;
const RIDE_STEPS: usize = 32;

struct Capture {
    pcm: Vec<f32>,
    failures: Vec<String>,
}

async fn render_solo(case: SyncCase, provider: Provider, audible_deck: usize) -> Capture {
    let mut harness = ProductHarness::new(case, provider, audible_deck).await;
    let pcm = render_frames(&mut harness, case, CAPTURE_FRAMES).await;
    Capture {
        pcm,
        failures: harness.failures,
    }
}

async fn render_mix(case: SyncCase, provider: Provider, target_bpm: Option<f64>) -> Capture {
    let mut harness = ProductHarness::new(case, provider, 0).await;
    for deck in &harness.decks {
        deck.set_muted(false);
    }
    harness.request_sync(case).await;

    let pcm = if let Some(target_bpm) = target_bpm {
        let mut pcm = Vec::with_capacity(CAPTURE_FRAMES * usize::from(CHANNELS));
        let mut rendered = 0;
        for step in 1..=RIDE_STEPS {
            let progress = step as f64 / RIDE_STEPS as f64;
            harness.set_tempo(case, (target_bpm - 120.0).mul_add(progress, 120.0), false);
            let deadline = CAPTURE_FRAMES * step / RIDE_STEPS;
            pcm.extend(render_frames(&mut harness, case, deadline - rendered).await);
            rendered = deadline;
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

async fn render_frames(harness: &mut ProductHarness, case: SyncCase, frames: usize) -> Vec<f32> {
    let mut pcm = Vec::with_capacity(frames * usize::from(CHANNELS));
    let mut rendered = 0;
    while rendered < frames {
        let block_frames = (frames - rendered).min(BLOCK_FRAMES);
        let block = harness.render(case, block_frames).await;
        assert_eq!(
            block.len(),
            block_frames * usize::from(CHANNELS),
            "offline renderer must return the requested complete block",
        );
        pcm.extend_from_slice(&block);
        rendered += block_frames;
    }
    pcm
}

fn write_capture(artifacts: &AudioArtifactSet, label: &str, pcm: &[f32]) -> PathBuf {
    let frames = pcm.len() / usize::from(CHANNELS);
    let mut recording = artifacts
        .recording(label, Some(frames as u64))
        .unwrap_or_else(|error| panic!("open {label} recording: {error}"));
    recording
        .push(pcm)
        .unwrap_or_else(|error| panic!("record {label}: {error}"));
    let reader = AudioArtifactSet::finish(recording)
        .unwrap_or_else(|error| panic!("finish {label} recording: {error}"));
    audio_artifact_path(&reader)
        .unwrap_or_else(|error| panic!("resolve {label} artifact path: {error}"))
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(60))
)]
async fn sync_listening_mix_is_not_quieter_than_a_solo_deck() {
    let case = DOWNTEMPO_HOUSE_SYNC;
    let provider = DOWNTEMPO_HOUSE_PROVIDER;
    let mut decks = Vec::with_capacity(case.decks());
    for deck in 0..case.decks() {
        let capture = render_solo(case, provider, deck).await;
        decks.push(CochleaReport::measure(
            &capture.pcm,
            CHANNELS,
            case.sample_rate,
        ));
    }
    let mix = render_mix(case, provider, None).await;
    let mix = CochleaReport::measure(&mix.pcm, CHANNELS, case.sample_rate);
    let mut failures = mix_loudness_failures(case.id(), &mix, &decks, LOUDNESS_TOLERANCE_LU);
    if mix.clipped_samples > 0 || mix.true_peak_over_0dbtp {
        failures.push(format!("{}: mix clips: {mix:?}", case.id()));
    }
    assert!(
        failures.is_empty(),
        "sync listening loudness failed:\n{}\ndecks={decks:?}\nmix={mix:?}",
        failures.join("\n"),
    );
}

#[kithara::test(
    native,
    tokio,
    multi_thread,
    serial,
    flash(false),
    timeout(Duration::from_secs(300))
)]
#[ignore = "writes opt-in listening WAVs; ignored-red until Warp alignment is implemented"]
#[case::synthetic_120("synthetic-120", SEQUENTIAL_SYNC, Provider::Synthetic, None)]
#[case::synthetic_127("synthetic-127", SEQUENTIAL_SYNC, Provider::Synthetic, Some(127.0))]
#[case::sweep_145("sweep-145", SEQUENTIAL_SYNC, Provider::Sweep, Some(145.0))]
#[case::ambient_trip_hop(
    "ambient-dub-62-trip-hop-74",
    AMBIENT_TRIP_HOP_SYNC,
    AMBIENT_TRIP_HOP_PROVIDER,
    None
)]
#[case::downtempo_house(
    "downtempo-96-house-124",
    DOWNTEMPO_HOUSE_SYNC,
    DOWNTEMPO_HOUSE_PROVIDER,
    None
)]
#[case::techno_breakbeat(
    "techno-132-breakbeat-140",
    TECHNO_BREAKBEAT_SYNC,
    TECHNO_BREAKBEAT_PROVIDER,
    None
)]
#[case::cross_style_four_deck(
    "ambient-62-downtempo-96-house-124-breakbeat-140",
    CROSS_STYLE_SYNC,
    CROSS_STYLE_PROVIDER,
    None
)]
async fn record_sync_listening_wavs(
    #[case] artifact_case: &str,
    #[case] case: SyncCase,
    #[case] provider: Provider,
    #[case] target_bpm: Option<f64>,
) {
    let artifacts = AudioArtifactSet::from_env(artifact_case, case.sample_rate, CHANNELS)
        .expect("configure sync listening artifacts")
        .unwrap_or_else(|| {
            panic!("KITHARA_AUDIO_ARTIFACT_DIR must be set for the listening recorder")
        });
    let mut paths = Vec::with_capacity(case.decks() + 1);
    let mut deck_reports = Vec::with_capacity(case.decks());
    let mut failures = Vec::new();
    for deck in 0..case.decks() {
        let label = format!("deck-{}", deck + 1);
        let capture = render_solo(case, provider, deck).await;
        let path = write_capture(&artifacts, &label, &capture.pcm);
        deck_reports.push(CochleaReport::measure(
            &capture.pcm,
            CHANNELS,
            case.sample_rate,
        ));
        paths.push((label, path));
        failures.extend(capture.failures);
    }
    let mix = render_mix(case, provider, target_bpm).await;
    let mix_path = write_capture(&artifacts, "mix", &mix.pcm);
    let mix_report = CochleaReport::measure(&mix.pcm, CHANNELS, case.sample_rate);
    paths.push(("mix".to_owned(), mix_path));
    failures.extend(mix.failures);
    failures.extend(mix_loudness_failures(
        case.id(),
        &mix_report,
        &deck_reports,
        LOUDNESS_TOLERANCE_LU,
    ));
    if mix_report.clipped_samples > 0 || mix_report.true_peak_over_0dbtp {
        failures.push(format!("{}: mix clips: {mix_report:?}", case.id()));
    }

    let manifest = serde_json::json!({
        "case": case.id(),
        "fixture": artifact_case,
        "sample_rate": case.sample_rate,
        "channels": CHANNELS,
        "capture_frames": CAPTURE_FRAMES,
        "failures": failures,
        "cochlea": {
            "decks": deck_reports,
            "mix": mix_report,
        },
        "artifacts": paths.iter().map(|(label, path)| {
            serde_json::json!({ "label": label, "path": path })
        }).collect::<Vec<_>>(),
    });
    let manifest = artifacts
        .write_manifest(&manifest)
        .expect("write sync listening manifest");
    let manifest_path =
        audio_artifact_path(&manifest).expect("resolve sync listening manifest path");

    for (label, path) in &paths {
        eprintln!("KITHARA_AUDIO_ARTIFACT {label}: {}", path.display());
    }
    eprintln!(
        "KITHARA_AUDIO_ARTIFACT manifest: {}",
        manifest_path.display()
    );
    assert!(
        failures.is_empty(),
        "{} listening capture failed:\n{}",
        case.id(),
        failures.join("\n"),
    );
}
