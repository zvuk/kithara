//! Real-record regression tests for production beat analysis.
//!
//! Set `KITHARA_TEMPO_RECORDS` to `/path=bpm;/path=bpm` before running ignored tests.

#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    audio::{BeatGrid, analysis::BeatAnalysisConfig},
    bufpool::{BytePool, PcmPool},
    platform::{CancelToken, time::Duration},
    prelude::ResourceConfig,
};
use kithara_app::waveform::{TrackAnalysis, TrackAnalysisRunner};
use kithara_integration_tests::memory_asset_store;
use num_traits::ToPrimitive;

struct Consts;

impl Consts {
    // The envelope is incidental; beat analysis is the subject.
    const BUCKETS: usize = 2_000;
    // Allows detector drift while rejecting the nearest wrong meter ratios.
    const KNOWN_BPM_RATIO_TOLERANCE: f64 = 0.10;
    // Allows detector quantization while requiring scalar-marker agreement.
    const MARCH_RATIO_TOLERANCE: f64 = 0.02;
    const SECONDS_PER_MINUTE: f64 = 60.0;
}

fn records() -> Vec<(String, f64)> {
    let spec = std::env::var("KITHARA_TEMPO_RECORDS").unwrap_or_else(|_| {
        panic!("KITHARA_TEMPO_RECORDS must name records as /path=bpm;/path=bpm")
    });
    let records = spec
        .split(';')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let (path, bpm) = entry
                .rsplit_once('=')
                .unwrap_or_else(|| panic!("{entry} must read /path=bpm"));
            let bpm: f64 = bpm
                .trim()
                .parse()
                .unwrap_or_else(|error| panic!("{entry} names no BPM: {error}"));
            (path.trim().to_owned(), bpm)
        })
        .collect::<Vec<_>>();
    assert!(
        !records.is_empty(),
        "KITHARA_TEMPO_RECORDS must name at least one /path=bpm record"
    );
    records
}

async fn analyse(path: &str) -> TrackAnalysis {
    let src = ResourceConfig::parse_src(path)
        .unwrap_or_else(|error| panic!("{path} must name a source: {error}"));
    let config = ResourceConfig::for_src(src)
        .store(memory_asset_store())
        .byte_pool(BytePool::default())
        .pcm_pool(PcmPool::default())
        .build();

    let mut runner = TrackAnalysisRunner::new(
        &CancelToken::never(),
        Consts::BUCKETS,
        BeatAnalysisConfig::default(),
        PcmPool::default(),
    );
    let mut rx = runner.analyze(config);

    // The runner emits the envelope before the beat grid.
    let mut last = None;
    while rx.changed().await.is_ok() {
        last = rx.borrow().clone();
    }
    last.unwrap_or_else(|| panic!("{path} produced no analysis at all"))
}

async fn grid_of(path: &str) -> (BeatGrid, f64) {
    let analysis = analyse(path).await;
    let rate = analysis
        .source_sample_rate()
        .unwrap_or_else(|| panic!("{path} carries no source rate"));
    let grid = analysis
        .beat()
        .unwrap_or_else(|| panic!("{path} produced no beat grid"))
        .clone();
    (grid, f64::from(rate.get()))
}

fn marker_tempo(grid: &BeatGrid, rate: f64) -> Option<f64> {
    let beats = grid.beats();
    let bpm = grid.bpm();
    if !rate.is_finite() || rate <= 0.0 || !bpm.is_finite() || bpm <= 0.0 {
        return None;
    }
    let beat_frames = Consts::SECONDS_PER_MINUTE * rate / bpm;
    if !beat_frames.is_finite() || beat_frames <= 0.0 {
        return None;
    }

    let mut frame_span = 0.0;
    let mut beat_span = 0.0;
    for pair in beats.windows(2) {
        let gap = pair[1].checked_sub(pair[0])?.to_f64()?;
        let beats_in_gap = (gap / beat_frames).round();
        if gap <= 0.0 || !beats_in_gap.is_finite() || beats_in_gap < 1.0 {
            return None;
        }
        frame_span += gap;
        beat_span += beats_in_gap;
    }

    let tempo = beat_span * Consts::SECONDS_PER_MINUTE * rate / frame_span;
    tempo.is_finite().then_some(tempo)
}

#[kithara::test]
fn missing_marker_counts_as_multiple_beat_spans() {
    let grid = BeatGrid::new(
        120.0,
        vec![0, 22_050, 66_150, 88_200],
        Vec::new(),
        Vec::new(),
    );

    let tempo = marker_tempo(&grid, 44_100.0).expect("markers name a tempo");
    assert!((tempo - grid.bpm()).abs() < f64::EPSILON);
}

#[kithara::test]
fn scalar_tempo_disagrees_with_retained_marker_ordinals() {
    let grid = BeatGrid::new(
        100.0,
        vec![0, 22_050, 66_150, 88_200],
        Vec::new(),
        Vec::new(),
    );

    let tempo = marker_tempo(&grid, 44_100.0).expect("markers name a tempo");
    assert!((tempo - 120.0).abs() < f64::EPSILON);
    assert!(((grid.bpm() / tempo) - 1.0).abs() > Consts::MARCH_RATIO_TOLERANCE);
}

#[ignore = "needs a music library named through KITHARA_TEMPO_RECORDS"]
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn the_reported_tempo_matches_the_known_record() {
    for (path, known) in records() {
        let (grid, rate) = grid_of(&path).await;
        println!(
            "{path}: reported {:.3} BPM against known {known:.2}, {} markers at {rate} Hz",
            grid.bpm(),
            grid.beats().len()
        );
        assert!(
            ((grid.bpm() / known) - 1.0).abs() <= Consts::KNOWN_BPM_RATIO_TOLERANCE,
            "{path} reports {:.3} BPM for a {known:.2} BPM record",
            grid.bpm()
        );
    }
}

#[ignore = "needs a music library named through KITHARA_TEMPO_RECORDS"]
#[kithara::test(tokio, timeout(Duration::from_secs(180)))]
async fn the_reported_tempo_is_the_tempo_the_markers_march_at() {
    for (path, _known) in records() {
        let (grid, rate) = grid_of(&path).await;
        let marched = marker_tempo(&grid, rate)
            .unwrap_or_else(|| panic!("{path} has too few markers to name a tempo"));
        println!(
            "{path}: reported {:.3} BPM, markers march at {marched:.3}",
            grid.bpm()
        );
        assert!(
            ((grid.bpm() / marched) - 1.0).abs() <= Consts::MARCH_RATIO_TOLERANCE,
            "{path} reports {:.3} BPM while its {} markers march at {marched:.3} BPM",
            grid.bpm(),
            grid.beats().len()
        );
    }
}
