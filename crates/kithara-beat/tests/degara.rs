#![cfg(feature = "dsp")]

//! Golden parity against a real Essentia `BeatTrackerDegara` run.
//!
//! Scores the signal-processing backend by F-measure over the ±70 ms MIR
//! window. The floor and the window were chosen from the fixture material
//! before the detector existed; the change's `design.md` records why.
mod common;

use std::path::{Path, PathBuf};

use common::{Score, f_measure, load_golden};
use kithara_beat::SpectralBeats;
use kithara_bufpool::testing::pools;
use kithara_test_utils::kithara;

const WINDOW: f64 = 0.070;
const MIN_F: f64 = 0.85;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load_pcm_fixture() -> Vec<f32> {
    let path = fixture("beat_test_mono_22050.f32le");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read PCM fixture {}: {e}", path.display()));
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn report(s: &Score) {
    eprintln!(
        "beats: F={:.4} matched {}/{} (ref {}) max_diff={:.1}ms mean_diff={:.1}ms",
        s.f_measure,
        s.matched,
        s.n_est,
        s.n_ref,
        s.max_matched_diff * 1000.0,
        s.mean_matched_diff * 1000.0,
    );
}

#[kithara::test(native, flash(false))]
fn essentia_parity() {
    let pcm = load_pcm_fixture();
    let raw = SpectralBeats::new(pools())
        .expect("a fresh region has room for the window")
        .analyze(&pcm)
        .expect("the analysis fits the region");
    let golden = load_golden(&fixture("golden_degara.json"));

    let detected: Vec<f32> = raw.beats.iter().map(|mark| mark.at).collect();
    let beats = f_measure(&golden.beats, &detected, WINDOW);
    report(&beats);

    assert!(
        raw.downbeats.is_empty(),
        "this tracker does not establish bar starts"
    );
    assert!(
        beats.f_measure >= MIN_F,
        "beat F-measure {:.4} < {MIN_F} @ {WINDOW}s vs golden_degara.json",
        beats.f_measure
    );
}
