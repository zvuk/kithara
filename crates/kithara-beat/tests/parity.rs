#![cfg(feature = "embed-small-model")]

//! Golden parity test.
//!
//! Feeds the pre-decoded mono 22 050 Hz PCM fixture through the full pipeline
//! (mel -> chunked inference -> peak picking) and asserts F-measure >= 0.99 at
//! the standard ±70 ms MIR window for beats and downbeats, versus a golden
//! pregenerated reference.
mod common;

use common::{WINDOW, f_measure, fixture, load_golden, load_pcm_fixture, report};
use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};
use kithara_bufpool::testing::pools;
use kithara_test_utils::kithara;

const SMALL_MIN_F: f64 = 0.99;

#[kithara::test(native, flash(false))]
fn python_parity_small_model() {
    let pcm = load_pcm_fixture("beat_test_mono_22050.f32le");
    let bt = BeatThis::builder()
        .mel_model(MEL_MODEL_BYTES)
        .beat_model(BEAT_MODEL_BYTES)
        .pools(pools())
        .build()
        .unwrap_or_else(|e| panic!("BeatThis::builder failed: {e}"));
    let raw = bt
        .analyze(&pcm)
        .unwrap_or_else(|e| panic!("analyze failed: {e}"));

    let golden = load_golden(&fixture("golden_small.json"));

    let detected: Vec<f32> = raw.beats.iter().map(|mark| mark.at).collect();
    let detected_downbeats: Vec<f32> = raw.downbeats.iter().map(|mark| mark.at).collect();
    assert!(
        raw.beats
            .iter()
            .chain(raw.downbeats.iter())
            .all(|mark| mark.confidence > 0.0 && mark.confidence < 1.0),
        "every detected mark carries a probability, never a certainty"
    );

    let beats = f_measure(&golden.beats, &detected, WINDOW);
    let downbeats = f_measure(&golden.downbeats, &detected_downbeats, WINDOW);
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
