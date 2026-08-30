#![cfg(feature = "dsp")]

//! Scores the signal-processing backend against the recorded `BeatTrackerDegara`
//! reference by F-measure over the ±70 ms MIR window. The floor and the window
//! were chosen from the fixture material before the detector existed;
//! `tests/fixtures/README.md` holds the reference's provenance.
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

fn load_pcm_fixture(name: &str) -> Vec<f32> {
    let path = fixture(name);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read PCM fixture {}: {e}", path.display()));
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn report(s: &Score, level: f64) {
    eprintln!(
        "beats: F={:.4} level={level:.3} matched {}/{} (ref {}) max_diff={:.1}ms mean_diff={:.1}ms",
        s.f_measure,
        s.matched,
        s.n_est,
        s.n_ref,
        s.max_matched_diff * 1000.0,
        s.mean_matched_diff * 1000.0,
    );
}

fn score(pcm: &str, golden: &str) -> (Score, f64) {
    let pcm = load_pcm_fixture(pcm);
    let raw = SpectralBeats::new(pools())
        .expect("a fresh region has room for the window")
        .analyze(&pcm)
        .expect("the analysis fits the region");
    assert!(
        raw.downbeats.is_empty(),
        "this tracker does not establish bar starts"
    );
    let golden = load_golden(&fixture(golden));

    let detected: Vec<f32> = raw.beats.iter().map(|mark| mark.at).collect();
    let tempo = |beats: &[f32]| -> f64 {
        let mut gaps: Vec<f64> = beats
            .windows(2)
            .map(|pair| f64::from(pair[1] - pair[0]))
            .collect();
        gaps.sort_by(f64::total_cmp);
        60.0 / gaps[gaps.len() / 2]
    };
    let level = tempo(&detected) / tempo(&golden.beats);
    (f_measure(&golden.beats, &detected, WINDOW), level)
}

#[kithara::test(native, flash(false))]
fn degara_parity() {
    let (beats, level) = score("beat_test_mono_22050.f32le", "golden_degara.json");
    report(&beats, level);
    assert!(
        beats.f_measure >= MIN_F,
        "beat F-measure {:.4} < {MIN_F} @ {WINDOW}s vs golden_degara.json",
        beats.f_measure
    );
}

/// The material the first fixture does not cover: a track the reference tracks
/// at one steady level throughout, where reading a submultiple is the failure
/// to catch. A level ratio away from 1 says the grid is on the wrong metrical
/// level, which an F-measure alone reports only as a low number.
#[kithara::test(native, flash(false))]
fn degara_parity_holds_the_metrical_level() {
    let (beats, level) = score("track_excerpt_mono_22050.f32le", "golden_degara_track.json");
    report(&beats, level);
    assert!(
        (0.95..=1.05).contains(&level),
        "grid is at {level:.3} times the reference tempo: a different metrical level"
    );
    assert!(
        beats.f_measure >= MIN_F,
        "beat F-measure {:.4} < {MIN_F} @ {WINDOW}s vs golden_degara_track.json",
        beats.f_measure
    );
}
