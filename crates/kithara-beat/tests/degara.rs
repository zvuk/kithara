#![cfg(feature = "dsp")]

mod common;

use common::{WINDOW, f_measure, fixture, load_golden, load_pcm_fixture, report};
use kithara_beat::{SpectralBeats, Tempo};
use kithara_bufpool::testing::pools;
use kithara_test_utils::kithara;
use num_traits::cast::ToPrimitive;

const MIN_F: f64 = 0.85;

/// Tempo ratios that put the grid on a metrical level of its own.
const MULTIPLES: [f64; 6] = [0.5, 2.0 / 3.0, 0.75, 4.0 / 3.0, 1.5, 2.0];
/// How close to a ratio counts as being on that level.
const NEAR: f64 = 0.05;

struct Pass;

impl Pass {
    const KEPT_SECONDS: usize = 28;
    const RATE: usize = 22_050;
    const READY_SECONDS: usize = 32;
    const WINDOW_SECONDS: usize = 30;
}

struct Window {
    at: f64,
    until: f64,
    beats: Vec<f32>,
}

fn seconds(frames: usize) -> f64 {
    frames.to_f64().unwrap_or(f64::MAX) / Pass::RATE.to_f64().unwrap_or(1.0)
}

fn tempo(beats: &[f32]) -> Option<f64> {
    let mut gaps: Vec<f64> = beats
        .windows(2)
        .map(|pair| f64::from(pair[1] - pair[0]))
        .collect();
    gaps.sort_by(f64::total_cmp);
    let median = gaps.get(gaps.len() / 2)?;
    (*median > 0.0).then(|| 60.0 / median)
}

fn between(beats: &[f32], from: f64, until: f64) -> Vec<f32> {
    beats
        .iter()
        .copied()
        .filter(|at| f64::from(*at) >= from && f64::from(*at) < until)
        .collect()
}

fn windows(pcm: &[f32], from: usize) -> Vec<Window> {
    let detector = SpectralBeats::new(pools(), Tempo::default())
        .expect("a fresh region has room for the window");
    let mut out = Vec::new();
    let mut at = from;
    while at < pcm.len() {
        let available = pcm.len() - at;
        let full = available >= Pass::READY_SECONDS * Pass::RATE;
        let end = if full {
            at + Pass::WINDOW_SECONDS * Pass::RATE
        } else {
            pcm.len()
        };
        let kept = seconds(if full {
            Pass::KEPT_SECONDS * Pass::RATE
        } else {
            available
        });
        let raw = detector
            .analyze(&pcm[at..end])
            .expect("the analysis fits the region");
        assert!(
            raw.downbeats.is_empty(),
            "this tracker does not establish bar starts"
        );

        let start = seconds(at);
        out.push(Window {
            at: start,
            until: start + kept,
            beats: raw
                .beats
                .iter()
                .filter(|mark| mark.at.is_finite() && mark.at >= 0.0 && f64::from(mark.at) < kept)
                .map(|mark| mark.at + start.to_f32().unwrap_or(f32::MAX))
                .collect(),
        });

        if !full {
            break;
        }
        at += Pass::KEPT_SECONDS * Pass::RATE;
    }
    out
}

fn shown(value: Option<f64>, digits: usize) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.digits$}"))
}

fn offender(windows: &[Window], golden: &[f32]) -> Option<(f64, f64, f64)> {
    let mut found = None;
    for window in windows {
        let reference = tempo(&between(golden, window.at, window.until));
        let measured = tempo(&window.beats);
        let level = measured
            .zip(reference)
            .map(|(measured, reference)| measured / reference);
        eprintln!(
            "  window at {:6.1}s: {:>7} BPM, reference {:>7} BPM, level {:>6}",
            window.at,
            shown(measured, 2),
            shown(reference, 2),
            shown(level, 3),
        );
        if let Some(level) = level
            && let Some(multiple) = MULTIPLES
                .into_iter()
                .find(|multiple| (level / multiple - 1.0).abs() <= NEAR)
            && found.is_none()
        {
            found = Some((window.at, level, multiple));
        }
    }
    found
}

fn parity(pcm: &str, name: &str, from_seconds: usize) {
    let pcm = load_pcm_fixture(pcm);
    let golden = load_golden(&fixture(name));
    assert!(
        golden.downbeats.is_empty(),
        "this reference records beats alone"
    );
    let windows = windows(&pcm, from_seconds * Pass::RATE);
    let covered = windows.last().map_or(0.0, |window| window.until);

    let detected: Vec<f32> = windows
        .iter()
        .flat_map(|w| w.beats.iter().copied())
        .collect();
    let reference = between(&golden.beats, seconds(from_seconds * Pass::RATE), covered);

    let score = f_measure(&reference, &detected, WINDOW);
    report("beats", &score);
    let offender = offender(&windows, &golden.beats);

    if let Some((at, level, multiple)) = offender {
        panic!(
            "the window at {at:.1}s reads {level:.3} times the reference tempo there, its {multiple:.3} multiple"
        );
    }
    assert!(
        score.f_measure >= MIN_F,
        "beat F-measure {:.4} < {MIN_F} @ {WINDOW}s vs {name}",
        score.f_measure,
    );
}

#[kithara::test(native, flash(false))]
fn degara_parity() {
    parity(
        "beat_test_mono_22050.f32le",
        "golden_degara_windowed.json",
        0,
    );
}

#[kithara::test(native, flash(false))]
fn degara_parity_holds_the_metrical_level() {
    parity(
        "track_excerpt_mono_22050.f32le",
        "golden_degara_track_windowed.json",
        0,
    );
}

#[kithara::test(native, flash(false))]
fn degara_parity_holds_at_another_alignment() {
    parity(
        "track_excerpt_mono_22050.f32le",
        "golden_degara_track_windowed_from7.json",
        7,
    );
}
