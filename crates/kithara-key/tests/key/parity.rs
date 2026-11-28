use kithara_key::Mode;
use kithara_test_utils::kithara;

use crate::recipe::{estimate_whole, goldens, render};

#[kithara::test(timeout(Long))]
fn every_tonal_fixture_matches_key_extractor() {
    const STRENGTH_TOLERANCE: f32 = 1e-3;
    const KEY_NAMES_FROM_A: [&str; 12] = [
        "A", "Bb", "B", "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab",
    ];

    let goldens = goldens();
    let mut misses = Vec::new();
    for recipe in &goldens.fixtures {
        let Some(golden) = &recipe.golden else {
            continue;
        };
        let estimate = estimate_whole(&render(recipe, goldens.sample_rate), goldens.sample_rate)
            .unwrap_or_else(|| panic!("{}: no key", recipe.name));
        let key = estimate.key();
        let name = KEY_NAMES_FROM_A[(usize::from(key.tonic.semitone()) + 3) % 12];
        let scale = match key.mode {
            Mode::Major => "major",
            Mode::Minor => "minor",
        };
        let delta = (estimate.strength() - golden.strength).abs();
        eprintln!(
            "{}: golden {} {} {:.6}, ours {name} {scale} {:.6}, delta {delta:.2e}",
            recipe.name,
            golden.key,
            golden.scale,
            golden.strength,
            estimate.strength()
        );
        if name != golden.key || scale != golden.scale || delta > STRENGTH_TOLERANCE {
            misses.push(recipe.name.as_str());
        }
    }
    assert!(misses.is_empty(), "fixtures off the reference: {misses:?}");
}
