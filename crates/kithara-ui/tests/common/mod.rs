use std::collections::BTreeMap;

use kithara_ui::{
    ids::{ControlKind, EndpointId},
    registry::{
        ControlCatalog, ControlKindDesc, EndpointCategory, EndpointDesc, EndpointRegistry,
        PropKind, ValueKind,
    },
    size::{Dim, SizeSpec},
};

pub(crate) const CONTROL_SIZE: f32 = 10.0;

fn control_size() -> SizeSpec {
    SizeSpec::new(Dim::Fixed(CONTROL_SIZE), Dim::Fixed(CONTROL_SIZE))
}

#[derive(Default)]
pub(crate) struct TestCatalog {
    kinds: BTreeMap<ControlKind, ControlKindDesc>,
}

impl TestCatalog {
    fn insert(&mut self, kind: &str, mut description: ControlKindDesc) {
        description.size = control_size();
        self.kinds.insert(ControlKind(kind.to_owned()), description);
    }
}

impl ControlCatalog for TestCatalog {
    fn kind(&self, kind: &ControlKind) -> Option<&ControlKindDesc> {
        self.kinds.get(kind)
    }
}

#[derive(Default)]
pub(crate) struct TestRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl TestRegistry {
    fn insert(&mut self, category: EndpointCategory, id: &str, description: EndpointDesc) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for TestRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(crate) fn player_catalog() -> TestCatalog {
    let mut catalog = TestCatalog::default();
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
        ControlKindDesc::new(Some(ValueKind::Scalar), None),
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

pub(crate) fn player_registry() -> TestRegistry {
    let mut registry = TestRegistry::default();
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.toggle_play",
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
