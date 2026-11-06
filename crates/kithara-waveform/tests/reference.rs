#![cfg(all(feature = "dsp", not(target_arch = "wasm32")))]
#![forbid(unsafe_code)]

use std::path::PathBuf;

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::analysis_fixtures::{analysis_pcm, waveform_mix};
use kithara_test_utils::{bufpool::pools, kithara};
use kithara_waveform::{AnalysisParams, WaveformAnalyzer};

mod consts {
    /// Columns one snapshot folds the band series into.
    pub(super) const BUCKETS: usize = 512;
    /// Rate the analysis fixtures are rendered at.
    pub(super) const RATE: u32 = 44_100;
    /// How far a band height may move and still match its reference.
    pub(super) const TOLERANCE: f32 = 1e-4;
    /// Set to record the references instead of checking them.
    pub(super) const UPDATE_REFERENCE: &str = "KITHARA_WAVEFORM_UPDATE_REFERENCE";
}

/// Low, mid and high heights of every column the default analysis draws.
fn heights(pcm: &[f32], channels: usize) -> Vec<[f32; 3]> {
    let pools = pools();
    let mut analyzer = WaveformAnalyzer::new(consts::RATE, AnalysisParams::default(), &pools)
        .expect("the analyzer builds");
    analyzer
        .push(&pools, pcm, channels, 0)
        .expect("the fixture fits the test region");
    let extent = u64::try_from(pcm.len() / channels).expect("the frame count fits u64");
    analyzer
        .snapshot(consts::BUCKETS, Some(extent))
        .buckets()
        .iter()
        .map(|bucket| [bucket.low(), bucket.mid(), bucket.high()])
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .join("tests/fixtures")
        .join(name)
}

/// Checks `heights` against `name` within [`consts::TOLERANCE`]; with
/// [`consts::UPDATE_REFERENCE`] set it records the reference instead.
fn matches_reference(heights: &[[f32; 3]], name: &str) {
    let path = fixture(name);
    if std::env::var_os(consts::UPDATE_REFERENCE).is_some() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .unwrap_or_else(|error| panic!("failed to create {}: {error}", dir.display()));
        }
        let text = serde_json::to_string(heights).expect("band heights serialize");
        std::fs::write(&path, text)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
        return;
    }

    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read reference {}: {error}", path.display()));
    let expected: Vec<[f32; 3]> = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("failed to parse reference {}: {error}", path.display()));
    assert_eq!(
        heights.len(),
        expected.len(),
        "column count differs from {name}"
    );
    for (index, (got, want)) in heights.iter().zip(&expected).enumerate() {
        for (height, recorded) in got.iter().zip(want) {
            assert!(
                (height - recorded).abs() <= consts::TOLERANCE,
                "column {index} reads {height}, the reference {recorded}, in {name}"
            );
        }
    }
}

#[kithara::test(native)]
fn a_full_spectrum_mix_matches_the_reference(waveform_mix: Vec<f32>) {
    matches_reference(&heights(&waveform_mix, 1), "reference_waveform_mix.json");
}

#[kithara::test(native)]
fn a_stereo_tone_matches_the_reference(analysis_pcm: &'static [f32]) {
    matches_reference(&heights(analysis_pcm, 2), "reference_analysis_pcm.json");
}
