use std::collections::BTreeMap;

use kithara_queue::TrackEntry;
use kithara_ui::module::BindingRef;

use crate::{state::UiState, waveform::TrackAnalysis};

pub(super) enum ReadValue<'a> {
    Bool(bool),
    Scalar(f64),
    Text(String),
    Waveform(Option<&'a TrackAnalysis>),
    Tracks(&'a [TrackEntry]),
}

pub(super) fn resolve<'a>(ui: &'a UiState, binding: &BindingRef) -> Option<ReadValue<'a>> {
    match binding {
        BindingRef::Telemetry { id, with }
            if id.0 == "deck.playback.playing" && deck_is_a(with) =>
        {
            Some(ReadValue::Bool(ui.playing))
        }
        BindingRef::Telemetry { id, with }
            if id.0 == "deck.playback.position_normalized" && deck_is_a(with) =>
        {
            Some(ReadValue::Scalar(position_normalized(ui)))
        }
        BindingRef::Telemetry { id, with }
            if id.0 == "deck.playback.waveform" && deck_is_a(with) =>
        {
            Some(ReadValue::Waveform(ui.analysis.as_ref()))
        }
        BindingRef::Telemetry { id, with } if id.0 == "deck.track.title" && deck_is_a(with) => {
            Some(ReadValue::Text(ui.track_name.clone()))
        }
        BindingRef::Parameter { id, .. } if id.0 == "player.output.volume" => {
            Some(ReadValue::Scalar(f64::from(ui.volume)))
        }
        BindingRef::Model { id, .. } if id.0 == "library.visible_tracks" => {
            Some(ReadValue::Tracks(&ui.tracks))
        }
        _ => None,
    }
}

pub(super) fn position_normalized(ui: &UiState) -> f64 {
    if ui.duration > 0.0 {
        (ui.position / ui.duration).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn deck_is_a(scope: &BTreeMap<String, String>) -> bool {
    scope.get("deck").is_some_and(|deck| deck == "a")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kithara_test_utils::kithara;
    use kithara_ui::{ids::EndpointId, module::BindingRef};

    use super::{ReadValue, resolve};
    use crate::state::UiState;

    fn telemetry(id: &str, deck: &str) -> BindingRef {
        BindingRef::Telemetry {
            id: EndpointId(id.to_owned()),
            with: BTreeMap::from([("deck".to_owned(), deck.to_owned())]),
        }
    }

    #[kithara::test]
    fn resolves_playing_for_deck_a() {
        let binding = telemetry("deck.playback.playing", "a");
        for playing in [false, true] {
            let mut ui = UiState::empty();
            ui.playing = playing;
            let Some(ReadValue::Bool(value)) = resolve(&ui, &binding) else {
                panic!("playing binding must resolve to bool");
            };
            assert_eq!(value, playing);
        }
    }

    #[kithara::test]
    fn normalized_position_is_zero_without_duration() {
        let binding = telemetry("deck.playback.position_normalized", "a");
        let mut ui = UiState::empty();
        ui.position = 12.0;

        let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding) else {
            panic!("position binding must resolve to scalar");
        };
        assert!(value.abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn normalized_position_is_clamped() {
        let binding = telemetry("deck.playback.position_normalized", "a");
        for (position, expected) in [(-2.0, 0.0), (25.0, 0.25), (120.0, 1.0)] {
            let mut ui = UiState::empty();
            ui.duration = 100.0;
            ui.position = position;
            let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding) else {
                panic!("position binding must resolve to scalar");
            };
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[kithara::test]
    fn resolves_volume_without_deck_scope() {
        let binding = BindingRef::Parameter {
            id: EndpointId("player.output.volume".to_owned()),
            with: BTreeMap::new(),
        };
        let mut ui = UiState::empty();
        ui.volume = 0.37;

        let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding) else {
            panic!("volume binding must resolve to scalar");
        };
        assert!((value - f64::from(ui.volume)).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn rejects_unknown_deck_scope() {
        let ui = UiState::empty();
        let binding = telemetry("deck.playback.playing", "b");

        assert!(resolve(&ui, &binding).is_none());
    }
}
