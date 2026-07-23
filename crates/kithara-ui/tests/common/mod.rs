use std::collections::BTreeMap;

use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

#[derive(Default)]
pub(crate) struct TestRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl TestRegistry {
    pub(crate) fn insert(
        &mut self,
        category: EndpointCategory,
        id: &str,
        description: EndpointDesc,
    ) {
        self.endpoints
            .insert((category, EndpointId(id.to_owned())), description);
    }
}

impl EndpointRegistry for TestRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

pub(crate) fn player_registry() -> TestRegistry {
    let mut registry = TestRegistry::default();
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.jump_back",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.jump_forward",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.transport.set_cue",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    for id in [
        "deck.transport.toggle_loop",
        "deck.transport.toggle_play",
        "deck.transport.toggle_reverse",
        "deck.transport.toggle_sync",
        "deck.view.zoom_in",
        "deck.view.zoom_out",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
        );
    }
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
    for id in [
        "deck.playback.looping",
        "deck.playback.reverse",
        "deck.playback.synced",
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.tempo",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
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
        "deck.view.zoom",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Model,
        "library.visible_tracks",
        EndpointDesc::new(ValueKind::TrackList),
    );
    registry
}
