use std::num::NonZeroU16;

use kithara_test_utils::kithara;

use super::{
    BeatGridError, BeatGridModel, BeatGridState, GridBeat, GridDownbeat, Meter, RawBeatGrid,
    SCHEMA_VERSION,
};
use crate::consts;

fn beat(ordinal: i16) -> GridBeat {
    GridBeat {
        at: consts::ORIGIN + f64::from(ordinal) * consts::PERIOD,
        ordinal: i64::from(ordinal),
        confidence: None,
    }
}

fn downbeat(ordinal: i16) -> GridDownbeat {
    GridDownbeat {
        at: beat(ordinal).at,
        beat_ordinal: i64::from(ordinal),
        confidence: None,
    }
}

fn raw(beats: Vec<GridBeat>, downbeats: Vec<GridDownbeat>) -> RawBeatGrid {
    RawBeatGrid {
        beats,
        downbeats,
        schema_version: SCHEMA_VERSION,
        model_id: "track-42".to_owned(),
        revision: 1,
        state: BeatGridState::Provisional,
        duration: Some(60.0),
        bpm: consts::BPM,
        meter: None,
    }
}

fn rejects(raw: RawBeatGrid) -> BeatGridError {
    BeatGridModel::try_from(raw).expect_err("the document contradicts itself")
}

/// The published example is the contract, field for field.
#[kithara::test(native, flash(false))]
fn the_documented_example_parses_into_the_grid_it_shows() {
    let document = r#"{
      "schema_version": 1,
      "model_id": "track-42",
      "revision": 1,
      "state": "provisional",
      "duration": 60.0,
      "bpm": 120.0,
      "beats": [
        {"at": 0.25, "ordinal": 0},
        {"at": 0.75, "ordinal": 1},
        {"at": 30.25, "ordinal": 60}
      ],
      "downbeats": [
        {"at": 0.25, "beat_ordinal": 0},
        {"at": 30.25, "beat_ordinal": 60}
      ],
      "meter": {"beats_per_bar": 4, "origin_beat_ordinal": 0}
    }"#;

    let model: BeatGridModel = serde_json::from_str(document).expect("the example is a grid");

    assert_eq!(model.as_raw().model_id, "track-42");
    assert_eq!(model.as_raw().revision, 1);
    assert_eq!(model.as_raw().state, BeatGridState::Provisional);
    assert_eq!(model.as_raw().duration, Some(60.0));
    assert_eq!(model.as_raw().bpm, consts::BPM);
    assert_eq!(
        model
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 60],
        "the ordinal is the beat's own number, not its place in the list"
    );
    assert_eq!(
        model.as_raw().meter.map(|meter| meter.beats_per_bar.get()),
        Some(4)
    );
}

/// A server document reaches a validated grid only through the checks.
#[kithara::test(native, flash(false))]
fn a_document_that_contradicts_itself_never_deserializes_into_a_grid() {
    let document = r#"{
      "schema_version": 1, "model_id": "track-42", "revision": 0,
      "state": "final", "bpm": 120.0,
      "beats": [{"at": 1.0, "ordinal": 1}, {"at": 0.5, "ordinal": 2}]
    }"#;

    assert!(
        serde_json::from_str::<BeatGridModel>(document).is_err(),
        "plain deserialization must not walk past the conversion"
    );
}

#[kithara::test(native, flash(false))]
fn a_grid_round_trips_through_its_own_serialization() {
    let model = BeatGridModel::try_from(raw(
        vec![beat(0), beat(1), beat(60)],
        vec![downbeat(0), downbeat(60)],
    ))
    .expect("a grid whose downbeats sit on its beats");

    let json = serde_json::to_string(&model).expect("a grid serializes");
    let back: BeatGridModel = serde_json::from_str(&json).expect("its own output is a grid");

    assert_eq!(back, model);
    assert!(
        !json.contains("confidence"),
        "an absent confidence stays absent rather than becoming zero: {json}"
    );
}

#[kithara::test(native, flash(false))]
fn an_unknown_length_is_absent_rather_than_zero() {
    let mut document = raw(vec![beat(0), beat(1)], Vec::new());
    document.duration = None;

    let model = BeatGridModel::try_from(document).expect("a grid may outlive a known length");
    let json = serde_json::to_string(&model).expect("a grid serializes");

    assert_eq!(model.as_raw().duration, None);
    assert!(
        !json.contains("duration"),
        "an unknown length is written as absent: {json}"
    );
}

/// BPM alone is a capability, and the smallest one a producer can publish.
#[kithara::test(native, flash(false))]
fn a_tempo_with_no_markers_is_a_grid() {
    let mut document = raw(Vec::new(), Vec::new());
    document.duration = None;

    let model = BeatGridModel::try_from(document).expect("a tempo is a claim on its own");

    assert_eq!(model.as_raw().bpm, consts::BPM);
    assert!(model.as_raw().beats.is_empty());
}

/// A grid that begins or ends mid-track keeps the ordinals of its own beats.
#[kithara::test(native, flash(false))]
fn islands_keep_their_ordinals_across_the_gap_between_them() {
    let model = BeatGridModel::try_from(raw(
        vec![beat(-4), beat(-3), beat(80), beat(81)],
        vec![downbeat(-4), downbeat(80)],
    ))
    .expect("two islands of a single grid");

    assert_eq!(
        model
            .as_raw()
            .beats
            .iter()
            .map(|beat| beat.ordinal)
            .collect::<Vec<_>>(),
        [-4, -3, 80, 81],
        "the gap costs no ordinals and the head reaches back before the origin"
    );
}

#[kithara::test(native, flash(false))]
fn a_tempo_no_detector_could_have_measured_is_refused() {
    for bpm in [0.0, -120.0, f64::NAN, f64::INFINITY] {
        let mut document = raw(Vec::new(), Vec::new());
        document.bpm = bpm;
        assert!(
            matches!(rejects(document), BeatGridError::Bpm { .. }),
            "{bpm} was accepted as a tempo"
        );
    }
}

#[kithara::test(native, flash(false))]
fn a_marker_outside_the_media_timeline_is_refused() {
    for at in [f64::NAN, f64::INFINITY, -0.5] {
        let mut mark = beat(1);
        mark.at = at;
        assert!(
            matches!(
                rejects(raw(vec![mark], Vec::new())),
                BeatGridError::Time { .. }
            ),
            "{at} s was accepted as a position"
        );
    }

    let mut past = beat(1);
    past.at = 60.5;
    assert!(matches!(
        rejects(raw(vec![past], Vec::new())),
        BeatGridError::PastDuration { .. }
    ));
}

#[kithara::test(native, flash(false))]
fn a_confidence_no_detector_could_have_reported_is_refused() {
    for confidence in [-0.1, 1.5, f32::NAN] {
        let mut mark = beat(0);
        mark.confidence = Some(confidence);
        assert!(
            matches!(
                rejects(raw(vec![mark], Vec::new())),
                BeatGridError::Confidence { .. }
            ),
            "{confidence} was accepted as a confidence"
        );
    }
}

#[kithara::test(native, flash(false))]
fn markers_that_do_not_advance_are_refused() {
    let repeated_time = vec![
        beat(0),
        GridBeat {
            ordinal: 1,
            ..beat(0)
        },
    ];
    assert!(matches!(
        rejects(raw(repeated_time, Vec::new())),
        BeatGridError::Order { .. }
    ));

    let repeated_ordinal = vec![
        beat(0),
        GridBeat {
            ordinal: 0,
            ..beat(1)
        },
    ];
    assert!(matches!(
        rejects(raw(repeated_ordinal, Vec::new())),
        BeatGridError::Order { .. }
    ));
}

#[kithara::test(native, flash(false))]
fn a_downbeat_that_names_no_beat_of_this_grid_is_refused() {
    assert!(
        matches!(
            rejects(raw(vec![beat(0), beat(1)], vec![downbeat(4)])),
            BeatGridError::Anchor { .. }
        ),
        "a bar line must fall on a beat the grid lists"
    );

    let moved = GridDownbeat {
        at: 0.3,
        ..downbeat(0)
    };
    assert!(
        matches!(
            rejects(raw(vec![beat(0), beat(1)], vec![moved])),
            BeatGridError::Anchor { .. }
        ),
        "a bar line must sit where its beat sits"
    );
}

#[kithara::test(native, flash(false))]
fn a_meter_that_disagrees_with_the_bar_lines_is_refused() {
    let mut document = raw(
        vec![beat(0), beat(1), beat(2), beat(3), beat(4)],
        vec![downbeat(0), downbeat(3)],
    );
    document.meter = Some(Meter {
        beats_per_bar: NonZeroU16::new(4).expect("invariant: four beats a bar"),
        origin_beat_ordinal: 0,
    });

    assert!(matches!(
        rejects(document),
        BeatGridError::Meter {
            beats_per_bar: 4,
            ..
        }
    ));
}

#[kithara::test(native, flash(false))]
fn a_document_from_another_schema_is_refused() {
    let mut document = raw(Vec::new(), Vec::new());
    document.schema_version = SCHEMA_VERSION + 1;
    assert!(matches!(rejects(document), BeatGridError::Schema { .. }));

    let mut nameless = raw(Vec::new(), Vec::new());
    nameless.model_id = String::new();
    assert!(matches!(rejects(nameless), BeatGridError::ModelId));
}
