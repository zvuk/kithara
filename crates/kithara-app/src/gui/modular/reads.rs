use std::collections::BTreeMap;

use kithara_queue::TrackEntry;
use kithara_ui::{compile::CompiledUi, expand::Binding, ids::InternId};

use crate::{state::UiState, waveform::TrackAnalysis};

pub(super) enum ReadValue<'a> {
    Bool(bool),
    Scalar(f64),
    Text(String),
    Waveform(Option<&'a TrackAnalysis>),
    Tracks(&'a [TrackEntry]),
}

pub(super) fn resolve<'a>(
    ui_state: &'a UiState,
    binding: &Binding,
    ui: &CompiledUi,
) -> Option<ReadValue<'a>> {
    match binding {
        Binding::Telemetry { id, with }
            if ui.resolve(*id) == "deck.playback.playing" && deck_is_a(with, ui) =>
        {
            Some(ReadValue::Bool(ui_state.playing))
        }
        Binding::Telemetry { id, with }
            if ui.resolve(*id) == "deck.playback.position_normalized" && deck_is_a(with, ui) =>
        {
            Some(ReadValue::Scalar(position_normalized(ui_state)))
        }
        Binding::Telemetry { id, with }
            if ui.resolve(*id) == "deck.playback.waveform" && deck_is_a(with, ui) =>
        {
            Some(ReadValue::Waveform(ui_state.analysis.as_ref()))
        }
        Binding::Telemetry { id, with }
            if ui.resolve(*id) == "deck.track.title" && deck_is_a(with, ui) =>
        {
            Some(ReadValue::Text(ui_state.track_name.clone()))
        }
        Binding::Parameter { id, .. } if ui.resolve(*id) == "player.output.volume" => {
            Some(ReadValue::Scalar(f64::from(ui_state.volume)))
        }
        Binding::Model { id, .. } if ui.resolve(*id) == "library.visible_tracks" => {
            Some(ReadValue::Tracks(&ui_state.tracks))
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

pub(super) fn deck_is_a(scope: &BTreeMap<InternId, InternId>, ui: &CompiledUi) -> bool {
    scope
        .iter()
        .any(|(key, value)| ui.resolve(*key) == "deck" && ui.resolve(*value) == "a")
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::{CompiledNode, CompiledUi, compile},
        expand::{Binding, ExpandedNode},
        source::{MemResolver, UiConfig},
    };

    use super::{ReadValue, resolve};
    use crate::{gui::modular::endpoints, state::UiState};

    fn compile_read(kind: &str, read: &str) -> (CompiledUi, Binding) {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "read.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "read",
                root: Module(instance: "deck-a", source: "read.kmodule.ron"))"#,
        );
        resolver.insert(
            "read.kmodule.ron",
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "read",
                    root: Control(id: "control", kind: "{kind}", read: {read}))"#
            ),
        );
        let ui = compile(
            "read.klayout.ron",
            &resolver,
            &endpoints::catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("read fixture must compile: {error}"));
        let binding = {
            let CompiledNode::Module { root, .. } = &ui.root else {
                panic!("read fixture must compile to a module");
            };
            let ExpandedNode::Control {
                read: Some(binding),
                ..
            } = &**root
            else {
                panic!("read fixture must contain a read binding");
            };
            binding.clone()
        };
        (ui, binding)
    }

    fn telemetry(id: &str, deck: &str, kind: &str) -> (CompiledUi, Binding) {
        compile_read(
            kind,
            &format!(r#"Telemetry(id: "{id}", with: {{ "deck": "{deck}" }})"#),
        )
    }

    #[kithara::test]
    fn resolves_playing_for_deck_a() {
        let (compiled, binding) = telemetry("deck.playback.playing", "a", "button");
        for playing in [false, true] {
            let mut ui = UiState::empty();
            ui.playing = playing;
            let Some(ReadValue::Bool(value)) = resolve(&ui, &binding, &compiled) else {
                panic!("playing binding must resolve to bool");
            };
            assert_eq!(value, playing);
        }
    }

    #[kithara::test]
    fn normalized_position_is_zero_without_duration() {
        let (compiled, binding) =
            telemetry("deck.playback.position_normalized", "a", "telemetry.scalar");
        let mut ui = UiState::empty();
        ui.position = 12.0;

        let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding, &compiled) else {
            panic!("position binding must resolve to scalar");
        };
        assert!(value.abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn normalized_position_is_clamped() {
        let (compiled, binding) =
            telemetry("deck.playback.position_normalized", "a", "telemetry.scalar");
        for (position, expected) in [(-2.0, 0.0), (25.0, 0.25), (120.0, 1.0)] {
            let mut ui = UiState::empty();
            ui.duration = 100.0;
            ui.position = position;
            let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding, &compiled) else {
                panic!("position binding must resolve to scalar");
            };
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[kithara::test]
    fn resolves_volume_without_deck_scope() {
        let (compiled, binding) = compile_read(
            "fader.horizontal",
            r#"Parameter(id: "player.output.volume")"#,
        );
        let mut ui = UiState::empty();
        ui.volume = 0.37;

        let Some(ReadValue::Scalar(value)) = resolve(&ui, &binding, &compiled) else {
            panic!("volume binding must resolve to scalar");
        };
        assert!((value - f64::from(ui.volume)).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn rejects_unknown_deck_scope() {
        let ui = UiState::empty();
        let (compiled, binding) = telemetry("deck.playback.playing", "b", "button");

        assert!(resolve(&ui, &binding, &compiled).is_none());
    }
}
