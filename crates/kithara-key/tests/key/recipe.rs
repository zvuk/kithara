use std::{f64::consts::PI, num::NonZeroUsize, path::PathBuf};

use kithara_key::{KeyEstimate, KeyEstimator};
use kithara_test_utils::bufpool::{Pools, pools};
use num_traits::cast::ToPrimitive;
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Goldens {
    pub(crate) sample_rate: u32,
    pub(crate) fixtures: Vec<Recipe>,
}

#[derive(Deserialize)]
pub(crate) struct Recipe {
    pub(crate) name: String,
    gain: f64,
    partials: Vec<f64>,
    chords: Vec<Chord>,
    pub(crate) golden: Option<Golden>,
}

#[derive(Deserialize)]
struct Chord {
    seconds: f64,
    notes: Vec<i32>,
}

#[derive(Deserialize)]
pub(crate) struct Golden {
    pub(crate) key: String,
    pub(crate) scale: String,
    pub(crate) strength: f32,
}

pub(crate) fn goldens() -> Goldens {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden_key.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("golden_key.json: {error}"))
}

pub(crate) fn rendered(name: &str) -> Vec<f32> {
    let goldens = goldens();
    let recipe = goldens
        .fixtures
        .iter()
        .find(|recipe| recipe.name == name)
        .unwrap_or_else(|| panic!("no recipe {name}"));
    render(recipe, goldens.sample_rate)
}

/// `record_key_golden.py`'s formula: per sample, the sum over the chord's
/// notes and then each partial `h` of `a_h sin(2 pi f (h + 1) n / rate)`.
pub(crate) fn render(recipe: &Recipe, sample_rate: u32) -> Vec<f32> {
    let rate = f64::from(sample_rate);
    let mut out = Vec::new();
    let mut n = 0.0;
    for chord in &recipe.chords {
        let frames = (chord.seconds * rate)
            .round()
            .to_usize()
            .expect("a chord lasts a whole number of samples");
        let frequencies = chord
            .notes
            .iter()
            .map(|&note| 440.0 * 2.0f64.powf(f64::from(note - 69) / 12.0))
            .collect::<Vec<_>>();
        for _ in 0..frames {
            let mut sample = 0.0;
            for &freq in &frequencies {
                let mut harmonic = 1.0;
                for amplitude in &recipe.partials {
                    sample += amplitude * (2.0 * PI * freq * harmonic * n / rate).sin();
                    harmonic += 1.0;
                }
            }
            out.push(
                (recipe.gain * sample)
                    .to_f32()
                    .expect("a rendered sample fits f32"),
            );
            n += 1.0;
        }
    }
    out
}

pub(crate) fn estimator(pools: &Pools, sample_rate: u32) -> KeyEstimator {
    KeyEstimator::new(sample_rate, pools).expect("the estimator builds")
}

pub(crate) fn frames(pcm: &[f32]) -> u64 {
    u64::try_from(pcm.len()).expect("the length fits u64")
}

pub(crate) fn estimate_whole(pcm: &[f32], sample_rate: u32) -> Option<KeyEstimate> {
    let pools = pools();
    let mut estimator = estimator(&pools, sample_rate);
    estimator
        .push(&pools, pcm, NonZeroUsize::MIN, 0)
        .expect("the track fits the pools");
    estimator
        .set_end(frames(pcm))
        .expect("the tail frames close");
    estimator.estimate()
}
