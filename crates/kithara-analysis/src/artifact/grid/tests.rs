use std::num::{NonZeroU16, NonZeroU32};

use kithara_beat::{
    BeatGridModel, BeatGridState, GridBeat, GridDownbeat, RawBeatGrid, SCHEMA_VERSION,
};
use kithara_platform::time::Duration;
use kithara_sync::Meter;
use kithara_test_utils::kithara;
use num_traits::cast::ToPrimitive;

use super::BeatGridUnavailable;
use crate::{
    BeatArtifact,
    artifact::{
        snapshot::{BeatSnapshot, BeatState},
        track::{AnalysisToken, TrackAnalysis},
    },
    consts,
};

fn analysis(
    rate: u32,
    beats: &[u64],
    downbeats: &[u64],
    extent: Option<u64>,
    state: BeatState,
) -> TrackAnalysis {
    artifact_analysis(
        rate,
        BeatArtifact::new(
            consts::BPM,
            beats.iter().map(|frame| (*frame, Some(1.0))).collect(),
            downbeats.iter().map(|frame| (*frame, Some(0.9))).collect(),
        ),
        extent,
        state,
    )
}

fn artifact_analysis(
    rate: u32,
    artifact: BeatArtifact,
    extent: Option<u64>,
    state: BeatState,
) -> TrackAnalysis {
    artifact_analysis_at(rate, artifact, extent, state, Some(Duration::ZERO))
}

fn artifact_analysis_at(
    rate: u32,
    artifact: BeatArtifact,
    extent: Option<u64>,
    state: BeatState,
    origin: Option<Duration>,
) -> TrackAnalysis {
    TrackAnalysis::builder()
        .maybe_media_origin(origin)
        .token(AnalysisToken::from("track-42"))
        .source_sample_rate(NonZeroU32::new(rate).expect("invariant: a fixture rate is set"))
        .beat(BeatSnapshot::new(artifact, state, Vec::new()))
        .maybe_extent(extent)
        .revision(7)
        .build()
}

fn grid(analysis: &TrackAnalysis) -> BeatGridModel {
    BeatGridModel::try_from(analysis).expect("the pass states a grid")
}

fn times(model: &BeatGridModel) -> Vec<(i64, f64)> {
    model
        .as_raw()
        .beats
        .iter()
        .map(|beat| (beat.ordinal, beat.at))
        .collect()
}

/// Detected frames become media seconds here and nowhere else.
#[kithara::test(native, flash(false))]
fn a_pass_publishes_its_beats_as_media_seconds_on_its_own_grid() {
    let beats: Vec<u64> = (0..4).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        Some(4 * consts::PERIOD_48),
        BeatState::Provisional,
    ));

    assert_eq!(
        model.as_raw().model_id,
        "track-42",
        "the token names the model"
    );
    assert_eq!(model.as_raw().revision, 7);
    assert_eq!(model.as_raw().state, BeatGridState::Provisional);
    assert_eq!(model.as_raw().bpm, consts::BPM);
    assert_eq!(
        model.as_raw().duration,
        Some(2.0),
        "the extent states the length"
    );
    assert_eq!(times(&model), [(0, 0.0), (1, 0.5), (2, 1.0), (3, 1.5)]);
}

fn tempos(model: &BeatGridModel) -> Vec<f64> {
    model
        .as_raw()
        .beats
        .windows(2)
        .map(|pair| 60.0 / (pair[1].at - pair[0].at))
        .collect()
}

/// A detector reads beats on its own frame clock, so a steady track
/// arrives with gaps a whole detector frame apart; the tempo is the
/// track's, and the rounding is not.
#[kithara::test(native, flash(false))]
fn beats_rounded_to_the_detector_clock_state_the_steady_tempo() {
    const DETECTOR_FPS: f64 = 50.0;
    const TEMPO: f64 = 120.02;
    let beats: Vec<u64> = (0..=100)
        .map(|beat| {
            let at = 0.013 + f64::from(beat) * 60.0 / TEMPO;
            let rounded = (at * DETECTOR_FPS).round() / DETECTOR_FPS;
            (rounded * f64::from(consts::RATE_48))
                .round()
                .to_u64()
                .expect("a beat before the media end is a frame")
        })
        .collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        beats.last().copied(),
        BeatState::Final,
    ));

    for tempo in tempos(&model) {
        assert!((tempo - TEMPO).abs() < 0.1, "a beat states {tempo} BPM");
    }
}

/// A tempo the music really changes is not rounding: each side keeps its own.
#[kithara::test(native, flash(false))]
#[case::sixteen(16u64)]
#[case::sixty_four(64u64)]
fn a_real_tempo_change_keeps_both_tempos(#[case] count: u64) {
    let slow = consts::PERIOD_48;
    let fast = consts::PERIOD_48 * 20 / 21;
    let beats: Vec<u64> = (0..count)
        .map(|beat| beat * slow)
        .chain((0..count).map(|beat| count * slow + beat * fast))
        .collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        None,
        BeatState::Final,
    ));

    let tempos = tempos(&model);
    assert!((tempos[0] - 120.0).abs() < 0.1, "opens at {}", tempos[0]);
    let end = tempos[tempos.len() - 1];
    assert!((end - 126.0).abs() < 0.1, "ends at {end}");
}

/// The model carries no sample rate, so two passes over the same music
/// must agree once their frames are read against their own rates.
#[kithara::test(native, flash(false))]
fn the_same_music_states_the_same_grid_from_either_source_rate() {
    let at_48: Vec<u64> = (0..4).map(|beat| beat * consts::PERIOD_48).collect();
    let at_44_1: Vec<u64> = (0..4).map(|beat| beat * consts::PERIOD_44_1).collect();

    assert_eq!(
        times(&grid(&analysis(
            consts::RATE_48,
            &at_48,
            &[],
            None,
            BeatState::Final
        ))),
        times(&grid(&analysis(
            consts::RATE_44_1,
            &at_44_1,
            &[],
            None,
            BeatState::Final
        )))
    );
}

/// The gap between two analysed islands costs the grid no beats.
#[kithara::test(native, flash(false))]
fn islands_keep_the_ordinals_the_music_gives_them() {
    let beats = [
        0,
        consts::PERIOD_48,
        60 * consts::PERIOD_48,
        61 * consts::PERIOD_48,
    ];
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        None,
        BeatState::Provisional,
    ));

    assert_eq!(
        model
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 60, 61],
        "the ordinal counts beats of the music, never entries of the list"
    );
}

/// A track a hair slower than its stated tempo drifts a whole beat off
/// the stated period within a few minutes; every beat it keeps is still
/// one beat after the one before it.
#[kithara::test(native, flash(false))]
fn a_tempo_drifting_from_the_stated_one_keeps_every_beat() {
    let slow = consts::PERIOD_48 + consts::PERIOD_48 / 100;
    let beats: Vec<u64> = (0..100).map(|beat| beat * slow).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        None,
        BeatState::Final,
    ));

    assert_eq!(
        model
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        (0..100).collect::<Vec<_>>()
    );
}

#[kithara::test(native, flash(false))]
#[case::stray_marker(1)]
#[case::offbeat_intro(2)]
#[case::wrong_detector_tempo(3)]
#[case::scattered_phrase(4)]
#[case::stated_tempo(5)]
fn a_steady_grid_fits_the_music_before_numbering_it(#[case] scenario: u8) {
    let period = consts::PERIOD_48;
    let beats: Vec<u64> = (0..256u64)
        .map(|beat| {
            let shift = match scenario {
                1 if beat == 100 => period / 5,
                2 if beat < 50 => period / 2,
                4 if (100..124).contains(&beat) => period * (beat % 7 + 1) / 10,
                _ => 0,
            };
            beat * period + shift
        })
        .collect();
    let reported = if scenario >= 2 { 121.2 } else { 120.0 };
    let model = grid(&artifact_analysis(
        consts::RATE_48,
        BeatArtifact::new(
            reported,
            beats.iter().map(|frame| (*frame, Some(1.0))).collect(),
            Vec::new(),
        ),
        None,
        BeatState::Final,
    ));
    let kept = &model.as_raw().beats;
    assert!(
        kept.len() >= if scenario == 2 { 206 } else { 220 },
        "the music survives the noisy phrase"
    );
    let first = kept.first().expect("steady beats survive");
    if scenario == 2 {
        assert!(
            (first.at - 25.0).abs() < 0.005,
            "the intro's offbeats are omitted"
        );
    }
    for beat in kept {
        let expected = first.at + (beat.ordinal - first.ordinal).to_f64().expect("ordinal") * 0.5;
        assert!(
            (beat.at - expected).abs() < 0.005,
            "beat {beat:?} left the tempo line"
        );
    }
    for pair in kept.windows(2) {
        let local = 60.0
            * (pair[1].ordinal - pair[0].ordinal)
                .to_f64()
                .expect("ordinal")
            / (pair[1].at - pair[0].at);
        assert!((local / 120.0 - 1.0).abs() < 1e-3, "local tempo {local}");
    }
    assert!(
        (model.as_raw().bpm - 120.0).abs() < 0.01,
        "stated tempo follows the line"
    );
}

fn ordinals(model: &BeatGridModel) -> Vec<i64> {
    model
        .as_raw()
        .beats
        .iter()
        .map(|beat| beat.ordinal)
        .collect()
}

fn half_beats(half_beats: impl Iterator<Item = u64>) -> Vec<u64> {
    half_beats
        .map(|half| half * consts::PERIOD_48 / 2)
        .collect()
}

/// A tracker that slips half a beat onto the off-beats for a phrase and
/// back names no whole beat there; the music on either side is still one
/// run of whole beats, counted across the slip.
#[kithara::test(native, flash(false))]
fn a_phrase_tracked_half_a_beat_off_leaves_the_beats_around_it_counted() {
    let beats = half_beats(
        (0..20)
            .map(|beat| 2 * beat)
            .chain((20..25).map(|beat| 2 * beat + 1))
            .chain((26..46).map(|beat| 2 * beat)),
    );
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        None,
        BeatState::Final,
    ));

    assert_eq!(ordinals(&model), (0..20).chain(26..46).collect::<Vec<_>>());
}

/// A stray first marker half a beat before the music names no beat of
/// it, and does not cost the music its beats.
#[kithara::test(native, flash(false))]
fn a_stray_first_marker_does_not_cost_the_music_its_beats() {
    let beats = half_beats(std::iter::once(0).chain((1..20).map(|beat| 2 * beat + 1)));
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        None,
        BeatState::Final,
    ));

    assert_eq!(ordinals(&model), (0..19).collect::<Vec<_>>());
    assert_eq!(
        model.as_raw().beats.first().map(|beat| beat.at),
        Some(0.75),
        "the grid opens on the music, not on the stray marker"
    );
}

/// A pass that does not yet know the length still states what it heard.
#[kithara::test(native, flash(false))]
fn an_unknown_length_does_not_withhold_the_grid() {
    let model = grid(&analysis(
        consts::RATE_48,
        &[0, consts::PERIOD_48],
        &[],
        None,
        BeatState::Provisional,
    ));

    assert_eq!(model.as_raw().duration, None);
    assert_eq!(model.as_raw().beats.len(), 2);
}

#[kithara::test(native, flash(false))]
fn a_later_pass_publishes_the_same_grid_as_final() {
    let beats = [0, consts::PERIOD_48];
    let provisional = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        Some(2 * consts::PERIOD_48),
        BeatState::Provisional,
    ));
    let final_pass = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        Some(2 * consts::PERIOD_48),
        BeatState::Final,
    ));

    assert_eq!(final_pass.as_raw().state, BeatGridState::Final);
    assert_eq!(
        times(&final_pass),
        times(&provisional),
        "settling changes the claim, not the beats"
    );
    assert_eq!(final_pass.as_raw().model_id, provisional.as_raw().model_id);
}

/// An artifact built entirely by extrapolation reports no tempo, and a
/// typed refusal is what a caller gets - the waveform is untouched.
#[kithara::test(native, flash(false))]
fn a_degraded_artifact_states_no_grid() {
    let degraded = artifact_analysis(
        consts::RATE_48,
        BeatArtifact::new(0.0, vec![(0, None), (100, None)], Vec::new()),
        Some(consts::PERIOD_48),
        BeatState::Final,
    );

    assert_eq!(
        BeatGridModel::try_from(&degraded),
        Err(BeatGridUnavailable::Tempo { bpm: 0.0 })
    );
}

#[kithara::test(native, flash(false))]
fn a_pass_without_a_beat_artifact_states_no_grid() {
    let waveform_only = TrackAnalysis::builder()
        .media_origin(Duration::ZERO)
        .token(AnalysisToken::from("track-42"))
        .source_sample_rate(
            NonZeroU32::new(consts::RATE_48).expect("invariant: a fixture rate is set"),
        )
        .revision(0)
        .build();

    assert_eq!(
        BeatGridModel::try_from(&waveform_only),
        Err(BeatGridUnavailable::NoBeats)
    );
}

/// A marker too far from a whole beat names none, so the grid leaves it
/// out rather than guessing an ordinal for it; the markers that do name a
/// beat follow the fitted line, with a gap where nothing was proved.
#[kithara::test(native, flash(false))]
fn a_marker_that_names_no_beat_is_left_out_of_the_grid() {
    let model = grid(&analysis(
        consts::RATE_48,
        &[0, consts::PERIOD_48, 40_000, 3 * consts::PERIOD_48],
        &[],
        None,
        BeatState::Provisional,
    ));

    assert_eq!(model.as_raw().bpm, consts::BPM);
    assert_eq!(
        model
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 3],
        "a guessed ordinal is not an observation, and its neighbours keep theirs"
    );
}

#[kithara::test(native, flash(false))]
fn the_bar_the_downbeats_keep_becomes_the_meter() {
    let beats: Vec<u64> = (0..9).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[0, 4 * consts::PERIOD_48, 8 * consts::PERIOD_48],
        None,
        BeatState::Final,
    ));

    assert_eq!(
        model
            .as_raw()
            .downbeats
            .iter()
            .map(|downbeat| downbeat.beat_ordinal)
            .collect::<Vec<_>>(),
        [0, 4, 8]
    );
    assert_eq!(
        model
            .as_raw()
            .meter
            .map(|meter| (meter.beats_per_bar.get(), meter.origin_beat_ordinal)),
        Some((4, 0))
    );
}

/// A bar line off every beat casts no vote, and the one left measures no
/// bar: the pass states no bars rather than a phase nothing repeats.
#[kithara::test(native, flash(false))]
fn one_placed_bar_line_measures_no_bar() {
    let beats: Vec<u64> = (0..5).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[0, 30_000],
        None,
        BeatState::Provisional,
    ));

    assert!(model.as_raw().downbeats.is_empty());
    assert_eq!(model.as_raw().meter, None);
    assert_eq!(
        model.as_raw().beats.len(),
        5,
        "the beats themselves still stand"
    );
}

fn downbeat_ordinals(model: &BeatGridModel) -> Vec<i64> {
    model
        .as_raw()
        .downbeats
        .iter()
        .map(|downbeat| downbeat.beat_ordinal)
        .collect()
}

/// A detector hears a bar line on the wrong beat now and then; the bars
/// around it outvote it instead of withdrawing the whole grid.
#[kithara::test(native, flash(false))]
fn a_bar_line_on_the_wrong_beat_is_outvoted() {
    let beats: Vec<u64> = (0..21).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[0, 4, 8, 10, 12, 16, 20].map(|beat| beat * consts::PERIOD_48),
        None,
        BeatState::Final,
    ));

    assert_eq!(downbeat_ordinals(&model), [0, 4, 8, 12, 16, 20]);
    assert_eq!(
        model
            .as_raw()
            .meter
            .map(|meter| (meter.beats_per_bar.get(), meter.origin_beat_ordinal)),
        Some((4, 0))
    );
}

/// A bar the detector skipped is still a bar: the phase the others agree
/// on states it, claiming no confidence of its own.
#[kithara::test(native, flash(false))]
fn a_bar_the_detector_skipped_is_stated_on_the_agreed_phase() {
    let beats: Vec<u64> = (0..17).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[0, 4, 12, 16].map(|beat| beat * consts::PERIOD_48),
        None,
        BeatState::Final,
    ));

    assert_eq!(downbeat_ordinals(&model), [0, 4, 8, 12, 16]);
    assert_eq!(
        model
            .as_raw()
            .downbeats
            .iter()
            .map(|downbeat| downbeat.confidence)
            .collect::<Vec<_>>(),
        [Some(0.9), Some(0.9), None, Some(0.9), Some(0.9)]
    );
}

/// Bar lines split evenly between two phases prove neither.
#[kithara::test(native, flash(false))]
fn bar_lines_split_between_two_phases_state_no_bar() {
    let beats: Vec<u64> = (0..15).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[0, 4, 10, 14].map(|beat| beat * consts::PERIOD_48),
        None,
        BeatState::Final,
    ));

    assert!(model.as_raw().downbeats.is_empty());
    assert_eq!(model.as_raw().meter, None);
}

/// Extrapolation stops where the media does.
#[kithara::test(native, flash(false))]
fn a_marker_past_the_stated_length_is_dropped_rather_than_published() {
    let beats: Vec<u64> = (0..4).map(|beat| beat * consts::PERIOD_48).collect();
    let model = grid(&analysis(
        consts::RATE_48,
        &beats,
        &[],
        Some(2 * consts::PERIOD_48),
        BeatState::Final,
    ));

    assert_eq!(times(&model), [(0, 0.0), (1, 0.5), (2, 1.0)]);
}
#[kithara::test]
fn a_raw_grid_with_an_origin_matches_the_imported_media_grid() {
    let imported = BeatGridModel::try_from(RawBeatGrid {
        duration: Some(4.5),
        bpm: 120.0,
        beats: (0..9)
            .map(|ordinal| GridBeat {
                at: f64::from(ordinal) * 0.5,
                ordinal: i64::from(ordinal),
                confidence: Some(1.0),
            })
            .collect(),
        downbeats: [0, 4, 8]
            .map(|ordinal| GridDownbeat {
                at: f64::from(ordinal) * 0.5,
                beat_ordinal: i64::from(ordinal),
                confidence: Some(0.9),
            })
            .to_vec(),
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(4).expect("four beats form a bar"),
            origin_beat_ordinal: 0,
        }),
        schema_version: SCHEMA_VERSION,
        model_id: "track-42".to_owned(),
        revision: 7,
        state: BeatGridState::Final,
    })
    .expect("the authored media grid is valid");
    let rate = NonZeroU32::new(44_100).expect("test rate is non-zero");
    let spec = kithara_signal::AudioSpec::new(1, rate);
    for offset in [529, 1105] {
        let raw = BeatArtifact::new(
            120.0,
            (0..9)
                .map(|beat| (offset + beat * 22_050, Some(1.0)))
                .collect(),
            [0, 4, 8]
                .map(|beat| (offset + beat * 22_050, Some(0.9)))
                .to_vec(),
        );
        let analysis = artifact_analysis_at(
            rate.get(),
            raw.clone(),
            Some(offset + 198_450),
            BeatState::Final,
            Some(spec.duration_for(offset).expect("origin is representable")),
        );
        assert_eq!(grid(&analysis), imported, "raw origin {offset}");
        assert_eq!(
            analysis.beat().expect("raw artifact survives").artifact(),
            &raw
        );
        assert_eq!(analysis.extent(), Some(offset + 198_450));
    }
}

#[kithara::test]
fn a_positive_first_native_position_is_not_rebased_as_media_zero() {
    let raw = BeatArtifact::new(
        120.0,
        [12_000, 36_000, 60_000]
            .map(|frame| (frame, Some(1.0)))
            .to_vec(),
        Vec::new(),
    );
    let analysis = artifact_analysis_at(
        48_000,
        raw,
        Some(96_000),
        BeatState::Final,
        Some(Duration::ZERO),
    );
    assert_eq!(times(&grid(&analysis)), [(0, 0.25), (1, 0.75), (2, 1.25)]);
    assert_eq!(grid(&analysis).as_raw().duration, Some(2.0));
}

#[kithara::test]
fn an_unknown_media_origin_refuses_a_grid_and_retains_raw_artifacts() {
    let raw = BeatArtifact::new(
        120.0,
        vec![(529, Some(1.0)), (22_579, Some(1.0))],
        Vec::new(),
    );
    let analysis = artifact_analysis_at(44_100, raw.clone(), Some(44_629), BeatState::Final, None);
    assert_eq!(
        BeatGridModel::try_from(&analysis),
        Err(BeatGridUnavailable::MediaOrigin)
    );
    assert_eq!(analysis.media_origin(), None);
    assert_eq!(analysis.extent(), Some(44_629));
    assert_eq!(
        analysis
            .beat()
            .expect("raw artifact remains available")
            .artifact(),
        &raw
    );
}

#[kithara::test]
fn only_the_public_projection_omits_markers_before_media_zero() {
    let raw = BeatArtifact::new(
        120.0,
        [0, 24_000, 48_000, 72_000]
            .map(|frame| (frame, Some(1.0)))
            .to_vec(),
        vec![(0, Some(0.9))],
    );
    let analysis = artifact_analysis_at(
        48_000,
        raw.clone(),
        Some(96_000),
        BeatState::Final,
        Some(Duration::from_millis(500)),
    );
    let public = grid(&analysis);
    assert_eq!(times(&public), [(0, 0.0), (1, 0.5), (2, 1.0)]);
    assert_eq!(public.as_raw().duration, Some(1.5));
    assert!(public.as_raw().downbeats.is_empty());
    assert_eq!(
        analysis.beat().expect("raw artifact survives").artifact(),
        &raw
    );
}

#[kithara::test]
fn an_extent_before_the_media_origin_is_a_typed_refusal() {
    let analysis = artifact_analysis_at(
        48_000,
        BeatArtifact::new(120.0, vec![(24_000, Some(1.0))], Vec::new()),
        Some(12_000),
        BeatState::Final,
        Some(Duration::from_millis(500)),
    );
    assert_eq!(
        BeatGridModel::try_from(&analysis),
        Err(BeatGridUnavailable::MediaExtent {
            origin: 24_000,
            end: 12_000,
        })
    );
}
