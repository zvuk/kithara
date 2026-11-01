use std::num::NonZeroU16;

use kithara_analysis::{
    BeatGridModel, BeatGridState, GRID_SCHEMA_VERSION, GridBeat, GridDownbeat, Meter, RawBeatGrid,
};
use kithara_test_macros as kithara;
use num_traits::AsPrimitive;

#[kithara::asset(
    ext = "json", content_type = "application/json", embed, format = synthetic_grid
)]
#[case::default()]
fn synthetic_grid() -> Vec<u8> {
    let spacing = 60.0 / 120.0;
    let model = BeatGridModel::try_from(RawBeatGrid {
        schema_version: GRID_SCHEMA_VERSION,
        model_id: "synthetic-rhythm".to_owned(),
        revision: 1,
        state: BeatGridState::Final,
        duration: Some(24.0 * spacing),
        bpm: 120.0,
        beats: (0..24_u32)
            .map(|ordinal| GridBeat {
                at: f64::from(ordinal) * spacing,
                ordinal: i64::from(ordinal),
                confidence: Some(1.0),
            })
            .collect(),
        downbeats: (1..24_u32)
            .step_by(4)
            .map(|ordinal| GridDownbeat {
                at: f64::from(ordinal) * spacing,
                beat_ordinal: i64::from(ordinal),
                confidence: Some(1.0),
            })
            .collect(),
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(4).expect("fixture meter"),
            origin_beat_ordinal: 1,
        }),
    })
    .expect("synthetic rhythm beats form a valid grid");
    serde_json::to_vec(&model).expect("synthetic grid serializes")
}

#[kithara::asset(ext = "json", content_type = "application/json", embed)]
#[case::default()]
fn ryabina_listening_grid() -> Vec<u8> {
    let beats: Vec<_> = (0..=512_i64)
        .map(|ordinal| {
            let seconds: f64 = ordinal.as_();
            let frame = (seconds * 60.0 / 132.0 * 48_000.0).round();
            GridBeat {
                at: frame / 48_000.0,
                ordinal,
                confidence: None,
            }
        })
        .collect();
    let model = BeatGridModel::try_from(RawBeatGrid {
        schema_version: GRID_SCHEMA_VERSION,
        model_id: "manual-listening-score:ryabina:132bpm:4-4:origin0:v1".to_owned(),
        revision: 1,
        state: BeatGridState::Provisional,
        duration: None,
        bpm: 132.0,
        downbeats: beats
            .iter()
            .filter(|beat| beat.ordinal.rem_euclid(4) == 0)
            .map(|beat| GridDownbeat {
                at: beat.at,
                beat_ordinal: beat.ordinal,
                confidence: None,
            })
            .collect(),
        beats,
        meter: Some(Meter {
            beats_per_bar: NonZeroU16::new(4).expect("four beats per candidate bar"),
            origin_beat_ordinal: 0,
        }),
    })
    .expect("the manual listening score is coherent");
    serde_json::to_vec(&model).expect("manual listening score serializes")
}
