use std::collections::BTreeMap;

use kithara_ui::{
    ids::{ControlKind, EndpointId},
    registry::{
        ControlCatalog, ControlKindDesc, EndpointCategory, EndpointDesc, EndpointRegistry,
        PropKind, ValueKind,
    },
};

#[derive(Default)]
pub(crate) struct AppCatalog {
    kinds: BTreeMap<ControlKind, ControlKindDesc>,
}

impl AppCatalog {
    fn insert(&mut self, kind: &str, description: ControlKindDesc) {
        self.kinds.insert(ControlKind(kind.to_owned()), description);
    }
}

impl ControlCatalog for AppCatalog {
    fn kind(&self, kind: &ControlKind) -> Option<&ControlKindDesc> {
        self.kinds.get(kind)
    }
}

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

pub(crate) fn catalog() -> AppCatalog {
    let mut catalog = AppCatalog::default();
    catalog.insert(
        "text",
        ControlKindDesc::new(Some(ValueKind::Text), None).with_prop("style", PropKind::Text),
    );
    catalog.insert(
        "button",
        ControlKindDesc::new(Some(ValueKind::Bool), Some(ValueKind::Trigger))
            .with_prop("label", PropKind::Text),
    );
    catalog.insert(
        "telemetry.scalar",
        ControlKindDesc::new(Some(ValueKind::Scalar), None).with_prop("format", PropKind::Text),
    );
    catalog.insert(
        "fader.horizontal",
        ControlKindDesc::new(Some(ValueKind::Scalar), Some(ValueKind::Scalar)),
    );
    catalog.insert(
        "waveform.mini",
        ControlKindDesc::new(Some(ValueKind::Waveform), Some(ValueKind::Scalar)),
    );
    catalog.insert(
        "track_list",
        ControlKindDesc::new(Some(ValueKind::TrackList), None),
    );
    catalog
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
        "deck.playback.waveform",
        EndpointDesc::new(ValueKind::Waveform).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.track.title",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
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
    registry
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{builtin, compile::compile, source::Limits};

    use super::*;

    #[kithara::test]
    fn builtin_presets_compile_against_app_registry() {
        for preset in [builtin::MICRO_PRESET, builtin::PLAYER_PRESET] {
            compile(
                preset,
                &builtin::resolver(),
                &catalog(),
                &registry(),
                &Limits::default(),
            )
            .unwrap_or_else(|error| panic!("{preset}: {error}"));
        }
    }
}
