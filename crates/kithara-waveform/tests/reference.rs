#![cfg(all(feature = "dsp", not(target_arch = "wasm32")))]
#![forbid(unsafe_code)]

use std::path::PathBuf;

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::analysis_fixtures::{analysis_pcm, waveform_mix};
use kithara_test_utils::{bufpool::pools, kithara};
use kithara_waveform::{AnalysisParams, Waveform, WaveformAnalyzer};

mod consts {
    pub(super) const BUCKETS: usize = 512;
    pub(super) const RATE: u32 = 44_100;
    pub(super) const UPDATE_REFERENCE: &str = "KITHARA_WAVEFORM_UPDATE_REFERENCE";
}

fn waveform(pcm: &[f32], channels: usize) -> Waveform {
    let pools = pools();
    let mut analyzer = WaveformAnalyzer::new(consts::RATE, AnalysisParams::default(), &pools)
        .expect("the analyzer builds");
    analyzer
        .push(&pools, pcm, channels, 0)
        .expect("the fixture fits the test region");
    let extent = u64::try_from(pcm.len() / channels).expect("the frame count fits u64");
    analyzer
        .snapshot(consts::BUCKETS, Some(extent))
        .expect("finite fixture")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .join("tests/fixtures")
        .join(name)
}

fn matches_reference(actual: &Waveform, name: &str) {
    let path = fixture(name);
    if std::env::var_os(consts::UPDATE_REFERENCE).is_some() {
        std::fs::write(&path, kithara_blob::to_bytes(actual)).expect("write waveform reference");
        return;
    }
    let bytes = std::fs::read(&path).expect("read waveform reference");
    let expected = Waveform::try_from(bytes.as_slice()).expect("versioned reference");
    assert_eq!(
        actual.len(),
        expected.len(),
        "column count differs from {name}"
    );
    assert_eq!(actual, &expected, "quantized columns differ from {name}");
}

#[kithara::test(native)]
fn a_full_spectrum_mix_matches_the_reference(waveform_mix: Vec<f32>) {
    matches_reference(&waveform(&waveform_mix, 1), "v2_reference_waveform_mix.bin");
}

#[kithara::test(native)]
fn a_stereo_tone_matches_the_reference(analysis_pcm: &'static [f32]) {
    matches_reference(&waveform(analysis_pcm, 2), "v2_reference_analysis_pcm.bin");
}

#[kithara::test(native)]
fn frozen_v1_reference_waveforms_are_version_errors() {
    for name in ["reference_analysis_pcm.json", "reference_waveform_mix.json"] {
        let text = std::fs::read_to_string(fixture(name)).expect("frozen v1 heights");
        let heights: Vec<[f32; 3]> = serde_json::from_str(&text).expect("v1 three-band rows");
        let mut bytes = 1_u32.to_le_bytes().to_vec();
        for height in heights.into_iter().flatten() {
            bytes.extend_from_slice(&height.to_le_bytes());
        }
        assert!(matches!(
            Waveform::try_from(bytes.as_slice()),
            Err(kithara_blob::BlobError::Version {
                found: 1,
                expected: 2
            })
        ));
    }
}
