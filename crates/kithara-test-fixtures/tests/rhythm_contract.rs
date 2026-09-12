#![cfg(not(target_arch = "wasm32"))]

use kithara_analysis::{AnalysisFile, AnalysisFingerprint, BeatArtifact};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::assets::by_name;
use kithara_test_utils::kithara;
use num_traits::cast;

const STYLES: [(&str, f64); 6] = [
    ("ambient_dub_62", 62.0),
    ("trip_hop_74", 74.0),
    ("downtempo_96", 96.0),
    ("house_124", 124.0),
    ("techno_132", 132.0),
    ("breakbeat_140", 140.0),
];
const CONTROLS: [&str; 4] = [
    "aligned",
    "one_frame_late",
    "one_beat_bar_late",
    "missing_beat",
];

struct Consts;

impl Consts {
    const ANALYSIS_FINGERPRINT: &str = "rhythm-fixture:v1";
    const BEAT_MARKER: i16 = 22_000;
    const DOWNBEAT_MARKER: i16 = 28_000;
    const MARKER_MATCH_RATIO: f64 = 0.75;
    const MARKER_TOLERANCE_BEATS: f64 = 0.12;
    const SAMPLE_RATE: f64 = 48_000.0;
    const SECONDS_PER_MINUTE: f64 = 60.0;
    const TEMPO_TOLERANCE_RATIO: f64 = 0.04;
    const TOTAL_FRAMES: u64 = 48_000 * 12;
    const WAV_HEADER_BYTES: usize = 44;
}

fn artifact(style: &str, control: &str) -> BeatArtifact {
    artifact_named("rhythm_expected_analysis", style, control)
}

fn artifact_named(prefix: &str, style: &str, control: &str) -> BeatArtifact {
    let name = format!("{prefix}_{style}_{control}");
    let asset = by_name(&name).unwrap_or_else(|| panic!("missing `{name}`"));
    assert_eq!(asset.entry().content_type, "application/x-kithara-analysis");
    let fingerprint = AnalysisFingerprint::new(Some(Consts::ANALYSIS_FINGERPRINT), None);
    let file = AnalysisFile::parse(asset.bytes(), &fingerprint)
        .unwrap_or_else(|error| panic!("decode `{name}`: {error}"));
    file.latest()
        .analysis()
        .beat()
        .unwrap_or_else(|| panic!("`{name}` has no beat analysis"))
        .artifact()
        .clone()
}

#[kithara::test(native, flash(false))]
fn six_styles_expose_every_rhythm_control() {
    for (style, bpm) in STYLES {
        for control in CONTROLS {
            let wav = format!("rhythm_wav_{style}_{control}");
            let wav = by_name(&wav).unwrap_or_else(|| panic!("missing `{wav}`"));
            assert_eq!(wav.entry().content_type, "audio/wav");
            assert!(wav.bytes().starts_with(b"RIFF"));

            let artifact = artifact(style, control);
            assert_eq!(artifact.bpm(), bpm);
            let (beats, downbeats) = exact_score_map(style, control);
            assert_eq!(artifact.beats(), beats, "{style}/{control}: beat map");
            assert_eq!(
                artifact.downbeats(),
                downbeats,
                "{style}/{control}: downbeat map"
            );
            assert_eq!(
                artifact.beats(),
                wav_markers(wav.bytes(), false),
                "{style}/{control}: beat map must match WAV markers"
            );
            assert_eq!(
                artifact.downbeats(),
                wav_markers(wav.bytes(), true),
                "{style}/{control}: downbeat map must match WAV markers"
            );

            let analyzed = artifact_named("rhythm_analyzed_analysis", style, control);
            assert!(analyzed.bpm().is_finite() && analyzed.bpm() > 0.0);
            assert!(!analyzed.beats().is_empty());
        }
    }
}

#[kithara::test(native, flash(false))]
fn aligned_styles_have_a_stereo_musical_bed() {
    for (style, _) in STYLES {
        let name = format!("rhythm_wav_{style}_aligned");
        let asset = by_name(&name).unwrap_or_else(|| panic!("missing `{name}`"));
        let ratio = stereo_side_ratio(asset.bytes());
        assert!(ratio > 0.0001, "{style}: stereo side ratio is only {ratio}");
    }
}

#[kithara::test(native, flash(false))]
fn score_truth_distinguishes_all_three_negative_controls() {
    for (style, _) in STYLES {
        let aligned = artifact(style, "aligned");
        let one_frame = artifact(style, "one_frame_late");
        let bar_phase = artifact(style, "one_beat_bar_late");
        let missing = artifact(style, "missing_beat");

        assert_eq!(aligned.beats().len(), one_frame.beats().len());
        assert!(
            aligned
                .beats()
                .iter()
                .zip(one_frame.beats())
                .all(|(expected, shifted)| shifted == &expected.saturating_add(1)),
            "{style}: one-frame control must move every beat by exactly one frame"
        );
        assert_eq!(aligned.beats(), bar_phase.beats());
        assert_ne!(aligned.downbeats(), bar_phase.downbeats());
        assert_eq!(aligned.beats().len(), missing.beats().len() + 1);
        assert!(
            missing
                .beats()
                .windows(2)
                .any(|pair| pair[1].saturating_sub(pair[0])
                    > aligned.beats()[1] - aligned.beats()[0]),
            "{style}: missing-beat control must leave a real hole"
        );
    }
}

#[kithara::test(native, flash(false))]
fn production_analysis_agrees_with_independent_score_truth() {
    for (style, bpm) in STYLES {
        for control in CONTROLS {
            let expected = artifact(style, control);
            let analyzed = artifact_named("rhythm_analyzed_analysis", style, control);
            let tempo_error = (analyzed.bpm() - bpm).abs();
            assert!(
                tempo_error <= bpm * Consts::TEMPO_TOLERANCE_RATIO,
                "{style}/{control}: analyzed BPM {} differs from score {bpm}",
                analyzed.bpm()
            );

            let tolerance = cast(
                (Consts::SAMPLE_RATE * Consts::SECONDS_PER_MINUTE / bpm
                    * Consts::MARKER_TOLERANCE_BEATS)
                    .round(),
            )
            .expect("invariant: fixture marker tolerance fits u64");
            assert_markers_agree(
                style,
                control,
                "beat",
                expected.beats(),
                analyzed.beats(),
                tolerance,
            );
        }
    }
}

#[kithara::test(native, flash(false))]
#[ignore = "production beat analysis does not yet classify shifted bar phase"]
fn production_analysis_tracks_score_downbeat_phase() {
    for (style, bpm) in STYLES {
        for control in CONTROLS {
            let expected = artifact(style, control);
            let analyzed = artifact_named("rhythm_analyzed_analysis", style, control);
            let tolerance = cast(
                (Consts::SAMPLE_RATE * Consts::SECONDS_PER_MINUTE / bpm
                    * Consts::MARKER_TOLERANCE_BEATS)
                    .round(),
            )
            .expect("invariant: fixture marker tolerance fits u64");
            assert_markers_agree(
                style,
                control,
                "downbeat",
                expected.downbeats(),
                analyzed.downbeats(),
                tolerance,
            );
        }
    }
}

fn assert_markers_agree(
    style: &str,
    control: &str,
    kind: &str,
    expected: &[u64],
    analyzed: &[u64],
    tolerance: u64,
) {
    assert!(!analyzed.is_empty(), "{style}/{control}: no {kind}s");
    let matched = analyzed
        .iter()
        .filter(|marker| {
            expected
                .iter()
                .any(|candidate| marker.abs_diff(*candidate) <= tolerance)
        })
        .count();
    let matched: f64 = cast(matched).expect("invariant: fixture marker count fits f64");
    let total: f64 = cast(analyzed.len()).expect("invariant: fixture marker count fits f64");
    let ratio = matched / total;
    assert!(
        ratio >= Consts::MARKER_MATCH_RATIO,
        "{style}/{control}: only {matched}/{} analyzed {kind}s match score within {tolerance} frames: expected={expected:?}, analyzed={analyzed:?}",
        analyzed.len()
    );
}

fn stereo_side_ratio(wav: &[u8]) -> f64 {
    let mut side = 0.0;
    let mut mid = 0.0;
    for frame in wav[Consts::WAV_HEADER_BYTES..].chunks_exact(4) {
        let left = f64::from(i16::from_le_bytes([frame[0], frame[1]]));
        let right = f64::from(i16::from_le_bytes([frame[2], frame[3]]));
        side += (left - right).powi(2);
        mid += (left + right).powi(2);
    }
    side / mid
}

fn exact_score_map(style: &str, control: &str) -> (Vec<u64>, Vec<u64>) {
    let beat_frames = match style {
        "ambient_dub_62" => 46_452,
        "trip_hop_74" => 38_919,
        "downtempo_96" => 30_000,
        "house_124" => 23_226,
        "techno_132" => 21_818,
        "breakbeat_140" => 20_571,
        _ => panic!("unknown rhythm style `{style}`"),
    };
    let first = beat_frames + u64::from(control == "one_frame_late");
    let downbeat_phase = usize::from(control == "one_beat_bar_late");
    let mut beats = Vec::new();
    let mut downbeats = Vec::new();
    for (index, frame) in (first..Consts::TOTAL_FRAMES)
        .step_by(usize::try_from(beat_frames).expect("beat period fits usize"))
        .enumerate()
    {
        if control == "missing_beat" && index == 5 {
            continue;
        }
        beats.push(frame);
        if index % 4 == downbeat_phase {
            downbeats.push(frame);
        }
    }
    (beats, downbeats)
}

fn wav_markers(wav: &[u8], downbeats_only: bool) -> Vec<u64> {
    wav[Consts::WAV_HEADER_BYTES..]
        .chunks_exact(4)
        .enumerate()
        .filter_map(|(frame, bytes)| {
            let left = i16::from_le_bytes([bytes[0], bytes[1]]);
            let right = i16::from_le_bytes([bytes[2], bytes[3]]);
            let marker = left == right
                && if downbeats_only {
                    left == Consts::DOWNBEAT_MARKER
                } else {
                    matches!(left, Consts::BEAT_MARKER | Consts::DOWNBEAT_MARKER)
                };
            marker.then(|| u64::try_from(frame).expect("fixture frame fits u64"))
        })
        .collect()
}
