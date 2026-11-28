#![cfg(all(feature = "dsp", not(target_arch = "wasm32")))]
#![forbid(unsafe_code)]

use std::{array, num::NonZeroU32, path::PathBuf};

#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::analysis_fixtures::{analysis_pcm, waveform_mix};
use kithara_test_utils::{bufpool::pools, kithara};
use kithara_waveform::{
    AnalysisParams, Bucket, WAVEFORM_BYTES_VERSION, Waveform, WaveformAnalyzer,
};
use num_traits::ToPrimitive;

mod consts {
    pub(super) const REFERENCE_COLUMNS: usize = 512;
    /// Header the frozen reference files were cut with; their bodies are the
    /// quantized display columns, independent of the waveform codec version.
    pub(super) const REFERENCE_VERSION: u32 = 2;
    pub(super) const RATE: u32 = 44_100;
    pub(super) const UPDATE_REFERENCE: &str = "KITHARA_WAVEFORM_UPDATE_REFERENCE";
}

fn quantized(bucket: Bucket) -> [u8; 8] {
    let (left, right) = (bucket.left(), bucket.right());
    array::from_fn(|band| {
        let height = if band < 4 {
            left[band]
        } else {
            right[band - 4]
        };
        (255.0 * height).round().to_u8().expect("normalized height")
    })
}

/// Per-side band peaks of native columns over the reference's equal-duration ranges.
fn columns(pcm: &[f32], channels: usize) -> Vec<[u8; 8]> {
    let pools = pools();
    let mut analyzer = WaveformAnalyzer::new(consts::RATE, AnalysisParams::default(), &pools)
        .expect("the analyzer builds");
    analyzer
        .push(&pools, pcm, channels, 0)
        .expect("the fixture fits the test region");
    let extent = u64::try_from(pcm.len() / channels).expect("the frame count fits u64");
    let wave = analyzer
        .snapshot(NonZeroU32::MIN, Some(extent))
        .expect("finite fixture");
    let total = wave.len();
    let count = consts::REFERENCE_COLUMNS.min(total);
    let mut out = vec![[0_u8; 8]; count];
    for (index, bucket) in wave.buckets().enumerate() {
        let range = &mut out[((index + 1) * count - 1) / total];
        let bytes = quantized(bucket);
        *range = array::from_fn(|band| range[band].max(bytes[band]));
    }
    out
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo test package directory"))
        .join("tests/fixtures")
        .join(name)
}

fn matches_reference(actual: &[[u8; 8]], name: &str) {
    let path = fixture(name);
    let mut encoded = consts::REFERENCE_VERSION.to_le_bytes().to_vec();
    encoded.extend(actual.iter().flatten());
    if std::env::var_os(consts::UPDATE_REFERENCE).is_some() {
        std::fs::write(&path, encoded).expect("write waveform reference");
        return;
    }
    let expected = std::fs::read(&path).expect("read waveform reference");
    assert_eq!(
        encoded.len(),
        expected.len(),
        "column count differs from {name}"
    );
    assert!(encoded == expected, "quantized columns differ from {name}");
}

#[kithara::test(native)]
fn a_full_spectrum_mix_matches_the_reference(waveform_mix: Vec<f32>) {
    matches_reference(&columns(&waveform_mix, 1), "v2_reference_waveform_mix.bin");
}

#[kithara::test(native)]
fn a_stereo_tone_matches_the_reference(analysis_pcm: &'static [f32]) {
    matches_reference(&columns(analysis_pcm, 2), "v2_reference_analysis_pcm.bin");
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
                expected: WAVEFORM_BYTES_VERSION
            })
        ));
    }
}
