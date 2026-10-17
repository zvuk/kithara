use std::num::NonZeroU16;

use kithara_beat::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, RawBeatGrid,
    SCHEMA_VERSION,
};
use kithara_signal::{AudioSpec, SignalError};
use kithara_sync::Meter;
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
    #[error("the source states no original media clock")]
    MediaOrigin,
    #[error(transparent)]
    SourceClock(#[from] SignalError),
    #[error("source extent {end} precedes media zero {origin}")]
    MediaExtent { origin: u64, end: u64 },
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
        let spec = AudioSpec::new(1, analysis.source_sample_rate());
        let origin = spec.frame_at(
            analysis
                .media_origin()
                .ok_or(BeatGridUnavailable::MediaOrigin)?,
        )?;
        let rate = f64::from(spec.sample_rate.get());
        let duration = analysis
            .extent()
            .map(|extent| {
                extent
                    .checked_sub(origin)
                    .map(|frames| seconds(frames, rate))
                    .ok_or(BeatGridUnavailable::MediaExtent {
                        origin,
                        end: extent,
                    })
            })
            .transpose()?;
        let placed = place(snapshot, rate, bpm, duration, origin);
        let (downbeats, meter) = bars(snapshot, &placed);
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

/// Pairs observed source-frame markers with beat ordinals counted from the first
/// kept beat. Whole-beat spacing from kept neighbours prevents accumulated tempo
/// drift; stray or off-beat markers are omitted and leave ordinal gaps, meaning
/// nothing is proved there. Anchoring in the longest whole-beat run prevents an
/// initial tracking slip from excluding the rest of the track.
fn place(
    snapshot: &BeatSnapshot,
    rate: f64,
    bpm: f64,
    duration: Option<f64>,
    origin: u64,
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
        .filter_map(|(frame, confidence)| {
            let frame_after_origin = frame.checked_sub(origin)?;
            let beat = GridBeat {
                at: seconds(frame_after_origin, rate),
                ordinal: 0,
                confidence: *confidence,
            };
            Some((*frame, beat))
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
fn bars(snapshot: &BeatSnapshot, placed: &[(u64, GridBeat)]) -> (Vec<GridDownbeat>, Option<Meter>) {
    let artifact = snapshot.artifact();
    let heard: Vec<(usize, Option<f32>)> = artifact
        .downbeats()
        .iter()
        .zip(artifact.downbeat_confidence().iter())
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
    use std::num::{NonZeroU16, NonZeroU32};

    use kithara_beat::{
        BeatGridModel, BeatGridState, GridBeat, GridDownbeat, RawBeatGrid, SCHEMA_VERSION,
    };
    use kithara_platform::time::Duration;
    use kithara_sync::Meter;
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
        let analysis =
            artifact_analysis_at(44_100, raw.clone(), Some(44_629), BeatState::Final, None);
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
}
