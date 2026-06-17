//! Golden parity test.
//!
//! Feeds the pre-decoded mono 22 050 Hz PCM fixture through the full pipeline
//! (mel -> chunked inference -> peak picking) and asserts F-measure >= 0.99 at
//! the standard ±70 ms MIR window for beats and downbeats, versus a golden
//! pregenerated reference.
mod common;

use std::path::{Path, PathBuf};

use common::{Score, f_measure, load_golden};
use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};

const WINDOW: f64 = 0.070;
const SMALL_MIN_F: f64 = 0.99;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load_pcm_fixture() -> Vec<f32> {
    let path = fixture("beat_test_mono_22050.f32le");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read PCM fixture {}: {e}", path.display()));
    assert_eq!(
        bytes.len() % 4,
        0,
        "raw f32le fixture length must be a multiple of 4"
    );
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn report(kind: &str, s: &Score) {
    eprintln!(
        "{kind}: F={:.4} matched {}/{} (ref {}) max_diff={:.1}ms mean_diff={:.1}ms",
        s.f_measure,
        s.matched,
        s.n_est,
        s.n_ref,
        s.max_matched_diff * 1000.0,
        s.mean_matched_diff * 1000.0,
    );
}

#[test]
fn python_parity_small_model() {
    let pcm = load_pcm_fixture();
    let mut bt = BeatThis::try_from((MEL_MODEL_BYTES, BEAT_MODEL_BYTES))
        .unwrap_or_else(|e| panic!("BeatThis::try_from failed: {e}"));
    let raw = bt
        .analyze(&pcm)
        .unwrap_or_else(|e| panic!("analyze failed: {e}"));

    let golden = load_golden(&fixture("golden_small.json"));

    let beats = f_measure(&golden.beats, &raw.beats, WINDOW);
    let downbeats = f_measure(&golden.downbeats, &raw.downbeats, WINDOW);
    report("beats", &beats);
    report("downbeats", &downbeats);

    assert!(
        beats.f_measure >= SMALL_MIN_F,
        "beat F-measure {:.4} < {SMALL_MIN_F} @ {WINDOW}s vs golden_small.json",
        beats.f_measure
    );
    assert!(
        downbeats.f_measure >= SMALL_MIN_F,
        "downbeat F-measure {:.4} < {SMALL_MIN_F} @ {WINDOW}s vs golden_small.json",
        downbeats.f_measure
    );
}
