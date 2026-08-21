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
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.playback.cached_normalized",
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
    insert_stream_endpoints(&mut registry);
    insert_bar_endpoints(&mut registry);
    insert_menu_endpoints(&mut registry);
    insert_clock_endpoints(&mut registry);
    registry
}

/// What the micro bar reads beside the deck: engine load and latency, and the
/// set the record cell watches.
fn insert_bar_endpoints(registry: &mut TestRegistry) {
    registry.insert(
        EndpointCategory::Telemetry,
        "engine.load",
        EndpointDesc::new(ValueKind::Scalar),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "engine.latency",
        EndpointDesc::new(ValueKind::Text),
    );
    registry.insert(
        EndpointCategory::Model,
        "ui.set.recording",
        EndpointDesc::new(ValueKind::Bool),
    );
    for id in ["ui.set.record_hint", "ui.set.record_time"] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "ui.set.toggle_record",
        EndpointDesc::new(ValueKind::Trigger),
    );
}

fn insert_menu_endpoints(registry: &mut TestRegistry) {
    for (id, kind) in [
        ("ui.menu.open", ValueKind::Bool),
        ("ui.window.can_open", ValueKind::Bool),
        ("ui.prefs.wave_follow", ValueKind::Bool),
        ("ui.prefs.autogain", ValueKind::Bool),
        ("ui.prefs.mono", ValueKind::Bool),
        ("ui.set.casting", ValueKind::Bool),
        ("ui.set.cast_hint", ValueKind::Text),
        ("ui.window.count", ValueKind::Text),
        ("ui.modules.title", ValueKind::Text),
        ("ui.modules.count", ValueKind::Text),
        ("ui.layouts.active", ValueKind::Text),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
    for (id, kind, scope) in [
        ("ui.menu.group_open", ValueKind::Bool, "group"),
        ("ui.menu.group_hidden", ValueKind::Bool, "group"),
        ("ui.window.active", ValueKind::Bool, "window"),
        ("ui.window.hidden", ValueKind::Bool, "window"),
        ("ui.window.close_hidden", ValueKind::Bool, "window"),
        ("ui.window.title", ValueKind::Text, "window"),
        ("ui.window.caption", ValueKind::Text, "window"),
        ("ui.module.on", ValueKind::Bool, "module"),
        ("ui.layout.selected", ValueKind::Bool, "layout"),
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(kind).with_scope(scope),
        );
    }
    for id in [
        "ui.menu.toggle",
        "ui.menu.close",
        "ui.window.open",
        "ui.window.toggle_full_screen",
        "ui.prefs.toggle_wave_follow",
        "ui.prefs.toggle_autogain",
        "ui.prefs.toggle_mono",
        "ui.set.toggle_cast",
        "ui.library.add_folder",
        "ui.settings.open",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    for (id, scope) in [
        ("ui.menu.toggle_group", "group"),
        ("ui.window.focus", "window"),
        ("ui.window.cycle_display", "window"),
        ("ui.window.close", "window"),
        ("ui.module.toggle", "module"),
        ("ui.layout.apply", "layout"),
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope(scope),
        );
    }
}

fn insert_clock_endpoints(registry: &mut TestRegistry) {
    for (id, kind) in [
        ("clock.open", ValueKind::Bool),
        ("clock.bpm", ValueKind::Text),
        ("clock.source", ValueKind::Text),
        ("clock.warning", ValueKind::Text),
        ("clock.family.step", ValueKind::Bool),
        ("clock.family.leap", ValueKind::Bool),
        ("clock.limit", ValueKind::Scalar),
        ("clock.tolerance", ValueKind::Scalar),
        ("clock.grid.quantize", ValueKind::Bool),
        ("clock.grid.division", ValueKind::Text),
        ("clock.grid.snap", ValueKind::Bool),
        ("clock.grid.click", ValueKind::Bool),
        ("clock.link.enabled", ValueKind::Bool),
        ("clock.link.peers", ValueKind::Text),
        ("clock.midi.input", ValueKind::Text),
        ("clock.midi.output", ValueKind::Text),
        ("clock.midi.send", ValueKind::Bool),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
    registry.insert(
        EndpointCategory::Parameter,
        "clock.tempo",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for (id, kind) in [
        ("clock.source.active", ValueKind::Bool),
        ("clock.source.name", ValueKind::Text),
        ("clock.source.tempo", ValueKind::Text),
        ("clock.source.mode", ValueKind::Text),
        ("clock.source.pulse", ValueKind::Text),
        ("clock.source.stretch", ValueKind::Text),
        ("clock.source.synced", ValueKind::Bool),
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(kind).with_scope("source"),
        );
    }
    for id in [
        "clock.toggle",
        "clock.close",
        "clock.nudge_up",
        "clock.nudge_down",
        "clock.family.step",
        "clock.family.leap",
        "clock.grid.toggle_quantize",
        "clock.grid.toggle_snap",
        "clock.grid.toggle_click",
        "clock.link.toggle",
        "clock.midi.toggle_send",
        "clock.tap",
        "clock.half",
        "clock.double",
        "clock.reset",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "clock.source.select",
        EndpointDesc::new(ValueKind::Trigger).with_scope("source"),
    );
}

pub(crate) fn insert_stream_endpoints(registry: &mut TestRegistry) {
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.stream.quality_hidden",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.quality",
        EndpointDesc::new(ValueKind::Text).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.quality_menu",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.toggle_quality_menu",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.variant_active",
        EndpointDesc::new(ValueKind::Bool)
            .with_scope("deck")
            .with_scope("variant"),
    );
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.stream.variant_hidden",
        EndpointDesc::new(ValueKind::Bool)
            .with_scope("deck")
            .with_scope("variant"),
    );
    for id in ["deck.stream.variant_label", "deck.stream.variant_sub"] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(ValueKind::Text)
                .with_scope("deck")
                .with_scope("variant"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.select_variant",
        EndpointDesc::new(ValueKind::Trigger)
            .with_scope("deck")
            .with_scope("variant"),
    );
}
