use std::collections::BTreeMap;

use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

#[derive(Default)]
pub(crate) struct AppRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl AppRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for AppRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(crate) fn registry() -> AppRegistry {
    let mut registry = AppRegistry::default();
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.toggle_play",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.prev",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.next",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.seek_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.playing",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.position_normalized",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.remaining_secs",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.position_secs",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.duration_secs",
        EndpointDesc::new(ValueKind::Scalar).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.waveform",
        EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.track.title",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.track.source_kind",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.track.key",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "player.output.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    registry.insert(
        EndpointCategory::Parameter,
        "player.output.volume",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.visible_tracks",
        EndpointDesc::new(ValueKind::TrackList),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.query",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.preset",
        EndpointDesc::new(ValueKind::Text),
    );
    registry
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        builtin,
        compile::compile,
        expand::ControlSpec,
        module::{DeckSummaryStyle, WaveStyle},
        size::{Dim, control_size},
        source::UiConfig,
    };

    use super::*;

    #[kithara::test]
    fn builtin_presets_compile_against_app_registry() {
        for preset in [builtin::MICRO_PRESET, builtin::PLAYER_PRESET] {
            compile(
                preset,
                &builtin::resolver(),
                &registry(),
                builtin::skin_doc(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{preset}: {error}"));
        }
    }

    #[kithara::test]
    fn micro_window_size_is_derived_from_control_specs() {
        let ui = compile(
            builtin::MICRO_PRESET,
            &builtin::resolver(),
            &registry(),
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));

        assert!(ui.size.w.min() > 0.0, "width min: {}", ui.size.w.min());
        assert!(ui.size.h.min() > 0.0, "height min: {}", ui.size.h.min());
    }

    #[kithara::test]
    fn player_window_size_is_derived_from_control_specs() {
        let ui = compile(
            builtin::PLAYER_PRESET,
            &builtin::resolver(),
            &registry(),
            builtin::skin_doc(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("player preset must compile: {error}"));

        assert!(ui.size.w.min() > 0.0, "width min: {}", ui.size.w.min());
        assert!(ui.size.h.min() > 0.0, "height min: {}", ui.size.h.min());
    }

    #[kithara::test]
    fn visual_controls_declare_intrinsic_sizes() {
        for spec in [
            ControlSpec::DeckSummary {
                style: DeckSummaryStyle::Default,
            },
            ControlSpec::Brand,
            ControlSpec::PresetSelector,
            ControlSpec::Time,
        ] {
            let size = control_size(&spec, builtin::skin_doc());
            assert!(size.w.min() > 0.0 || size.w == Dim::Fill);
            assert!(size.h.min() > 0.0, "{spec:?} height");
        }

        let waveform = control_size(
            &ControlSpec::Wave {
                style: WaveStyle::Default,
                badge: None,
            },
            builtin::skin_doc(),
        );
        assert!(waveform.h.min() >= 120.0);
    }
}
