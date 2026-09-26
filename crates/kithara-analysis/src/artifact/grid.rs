use std::num::NonZeroU16;

use kithara_beat::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid,
    SCHEMA_VERSION,
};
use num_traits::cast::ToPrimitive;
use thiserror::Error;

use super::{
    meter::voted_bar,
    snapshot::{BeatSnapshot, BeatState},
    track::TrackAnalysis,
};
use crate::consts;

/// How far a marker may sit from a whole beat and still name that beat without
/// a second reading. Past a quarter beat the nearest ordinal is a guess, and a
/// guess is not an observation, so the pass publishes the tempo alone instead.
pub const ORDINAL_TOLERANCE_BEATS: f64 = 0.25;

/// Why a pass states no grid a player could follow.
///
/// The pass keeps its own artifact either way: a grid it cannot state is not a
/// waveform it cannot publish.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum BeatGridUnavailable {
    #[error("the pass published no beat artifact")]
    NoBeats,
    #[error(transparent)]
    Rejected(#[from] BeatGridError),
    #[error("the artifact carries {bpm} where a tempo would be")]
    Tempo { bpm: f64 },
}

/// Restates one published pass as the server-side grid contract.
///
/// This is the only place source frames become media seconds: the artifact
/// keeps frames, and the model has no sample rate to read them against.
impl TryFrom<&TrackAnalysis> for BeatGridModel {
    type Error = BeatGridUnavailable;

    fn try_from(analysis: &TrackAnalysis) -> Result<Self, Self::Error> {
        let snapshot = analysis.beat().ok_or(BeatGridUnavailable::NoBeats)?;
        let bpm = snapshot.artifact().bpm();
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(BeatGridUnavailable::Tempo { bpm });
        }
        let rate = f64::from(analysis.source_sample_rate().get());
        let duration = analysis.extent().map(|extent| seconds(extent, rate));
        let placed = place(snapshot, rate, bpm, duration);
        let (downbeats, meter) = bars(snapshot, &placed, rate, duration);
        Ok(Self::try_from(RawBeatGrid {
            duration,
            bpm,
            downbeats,
            meter,
            schema_version: SCHEMA_VERSION,
            model_id: analysis.token().as_str().to_owned(),
            revision: analysis.revision(),
            state: match snapshot.state() {
                BeatState::Final => BeatGridState::Final,
                BeatState::Provisional => BeatGridState::Provisional,
            },
            beats: placed.iter().map(|(_, beat)| *beat).collect(),
        })?)
    }
}

/// The artifact's beats with the ordinal each one holds, still paired with the
/// source frame the bar lines name them by.
///
/// Each beat kept is the whole beats it sits after or before its kept
/// neighbour, so a tempo drifting from the stated one never adds up to a lost
/// beat. Where the tracker slips off the beats - a stray marker, a phrase
/// tracked on the off-beats - a marker names no whole beat from its kept
/// neighbour and is left out rather than named wrongly: the grid then states a
/// gap in its numbers, which is what a consumer following ordinals reads as
/// "nothing proved here", while the beats around it stand as observed. The
/// count starts inside the longest run of markers a whole beat apart, so a
/// slip at the start of the track does not leave the music after it out.
/// Ordinals count from the first beat kept.
fn place(
    snapshot: &BeatSnapshot,
    rate: f64,
    bpm: f64,
    duration: Option<f64>,
) -> Vec<(u64, GridBeat)> {
    let artifact = snapshot.artifact();
    let period = consts::SECONDS_PER_MINUTE / bpm;
    let whole_beats = |from: &GridBeat, to: &GridBeat| {
        let exact = (to.at - from.at) / period;
        let rounded = exact.round();
        if rounded < 1.0 || (exact - rounded).abs() > ORDINAL_TOLERANCE_BEATS {
            return None;
        }
        rounded.to_i64()
    };
    let markers: Vec<(u64, GridBeat)> = artifact
        .beats()
        .iter()
        .zip(artifact.beat_confidence())
        .map(|(frame, confidence)| {
            let beat = GridBeat {
                at: seconds(*frame, rate),
                ordinal: 0,
                confidence: *confidence,
            };
            (*frame, beat)
        })
        .filter(|(_, beat)| !duration.is_some_and(|duration| beat.at > duration))
        .collect();
    let Some(anchor) = longest_run_start(&markers, whole_beats) else {
        return Vec::new();
    };
    let mut kept: Vec<(u64, GridBeat)> = Vec::with_capacity(markers.len());
    let mut first = markers[anchor].1;
    for (frame, beat) in markers[..anchor].iter().rev() {
        if let Some(step) = whole_beats(beat, &first) {
            first = GridBeat {
                ordinal: first.ordinal - step,
                ..*beat
            };
            kept.push((*frame, first));
        }
    }
    kept.reverse();
    let mut last = markers[anchor].1;
    kept.push(markers[anchor]);
    for (frame, beat) in &markers[anchor + 1..] {
        if let Some(step) = whole_beats(&last, beat) {
            last = GridBeat {
                ordinal: last.ordinal + step,
                ..*beat
            };
            kept.push((*frame, last));
        }
    }
    for (_, beat) in &mut kept {
        beat.ordinal -= first.ordinal;
    }
    kept
}

/// Where the longest run of markers each a whole beat after the one before
/// it starts; the earliest such run on a tie.
fn longest_run_start(
    markers: &[(u64, GridBeat)],
    whole_beats: impl Fn(&GridBeat, &GridBeat) -> Option<i64>,
) -> Option<usize> {
    let mut start = 0;
    let mut best: Option<(usize, usize)> = None;
    for index in 0..markers.len() {
        let continues =
            index > 0 && whole_beats(&markers[index - 1].1, &markers[index].1).is_some();
        if !continues {
            start = index;
        }
        let length = index + 1 - start;
        if best.is_none_or(|(_, longest)| length > longest) {
            best = Some((start, length));
        }
    }
    best.map(|(start, _)| start)
}

/// The bar lines the detected ones agree on, each named by the beat it falls
/// on, and the meter they keep.
///
/// Only a detected bar line on a placed beat votes; the bar and phase the
/// votes agree on then name every placed beat of that phase a bar line, so a
/// bar the detector skipped is stated without a confidence of its own and a
/// bar line on the wrong beat is left out. Votes that agree on nothing state
/// no bars at all.
fn bars(
    snapshot: &BeatSnapshot,
    placed: &[(u64, GridBeat)],
    rate: f64,
    duration: Option<f64>,
) -> (Vec<GridDownbeat>, Option<Meter>) {
    let artifact = snapshot.artifact();
    let heard: Vec<(usize, Option<f32>)> = artifact
        .downbeats()
        .iter()
        .zip(artifact.downbeat_confidence().iter())
        .filter(|(frame, _)| duration.is_none_or(|duration| seconds(**frame, rate) <= duration))
        .filter_map(|(frame, confidence)| {
            let index = placed
                .binary_search_by_key(frame, |(placed, _)| *placed)
                .ok()?;
            Some((index, *confidence))
        })
        .collect();
    let votes = heard
        .iter()
        .filter(|(_, confidence)| confidence.is_some())
        .map(|(index, _)| placed[*index].1.ordinal);
    let Some((bar, phase)) = voted_bar(votes) else {
        return (Vec::new(), None);
    };
    let downbeats: Vec<GridDownbeat> = placed
        .iter()
        .enumerate()
        .filter(|(_, (_, beat))| beat.ordinal.rem_euclid(bar) == phase)
        .map(|(index, (_, beat))| GridDownbeat {
            at: beat.at,
            beat_ordinal: beat.ordinal,
            confidence: heard
                .binary_search_by_key(&index, |(heard, _)| *heard)
                .ok()
                .and_then(|found| heard[found].1),
        })
        .collect();
    let meter = bar
        .to_u16()
        .and_then(NonZeroU16::new)
        .zip(downbeats.first())
        .map(|(beats_per_bar, first)| Meter {
            beats_per_bar,
            origin_beat_ordinal: first.beat_ordinal,
        });
    (downbeats, meter)
}

/// A frame the timeline cannot represent becomes a position the contract
/// refuses, rather than one it silently rounds.
fn seconds(frame: u64, rate: f64) -> f64 {
    frame.to_f64().unwrap_or(f64::INFINITY) / rate
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_beat::{BeatGridModel, BeatGridState};
    use kithara_test_utils::kithara;

    use super::{BeatGridUnavailable, BeatSnapshot, BeatState, TrackAnalysis};
    use crate::{BeatArtifact, artifact::track::AnalysisToken, consts};

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
        TrackAnalysis::builder()
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
    /// beat stand as observed, with a gap where nothing was proved.
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
}
