use std::collections::BTreeMap;

use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

use super::consts::Consts;
use crate::{mock_mixer, mock_stress};

#[derive(Default)]
pub(crate) struct MockRegistry {
    endpoints: BTreeMap<(EndpointCategory, EndpointId), EndpointDesc>,
}

impl MockRegistry {
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

impl EndpointRegistry for MockRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints.get(&(category, id.clone()))
    }
}

fn insert_engine_endpoints(registry: &mut MockRegistry) {
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
}

fn insert_output_levels(registry: &mut MockRegistry) {
    registry.insert(
        EndpointCategory::Telemetry,
        "player.output.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
}

fn insert_deck_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("deck.transport.jump_back", ValueKind::Trigger),
        ("deck.transport.jump_forward", ValueKind::Trigger),
        ("deck.transport.set_cue", ValueKind::Trigger),
        ("deck.transport.toggle_loop", ValueKind::Trigger),
        ("deck.transport.toggle_play", ValueKind::Trigger),
        ("deck.transport.toggle_reverse", ValueKind::Trigger),
        ("deck.transport.toggle_sync", ValueKind::Trigger),
        ("deck.transport.seek_normalized", ValueKind::Scalar),
        ("deck.key.transpose_down", ValueKind::Trigger),
        ("deck.key.toggle_lock", ValueKind::Trigger),
        ("deck.key.transpose_up", ValueKind::Trigger),
        ("deck.view.zoom_in", ValueKind::Trigger),
        ("deck.view.zoom_out", ValueKind::Trigger),
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    for (id, kind) in [
        ("deck.playback.playing", ValueKind::Bool),
        ("deck.playback.position_normalized", ValueKind::Scalar),
        ("deck.playback.cached_normalized", ValueKind::Scalar),
        ("deck.playback.remaining_secs", ValueKind::Scalar),
        ("deck.playback.remain", ValueKind::Text),
        ("deck.playback.position_secs", ValueKind::Scalar),
        ("deck.playback.duration_secs", ValueKind::Scalar),
        ("deck.playback.looping", ValueKind::Bool),
        ("deck.playback.reverse", ValueKind::Bool),
        ("deck.playback.synced", ValueKind::Bool),
        ("deck.playback.tempo", ValueKind::Text),
        ("deck.playback.waveform", ValueKind::Waveform),
        ("deck.track.title", ValueKind::Text),
        ("deck.track.source_kind", ValueKind::Text),
        ("deck.track.key", ValueKind::Text),
        ("deck.focused", ValueKind::Bool),
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
}

fn insert_clock_endpoints(registry: &mut MockRegistry) {
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
        EndpointCategory::Model,
        "deck.key.locked",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
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

fn insert_pivot_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("pivot.map", ValueKind::PortalMap),
        ("pivot.master.label", ValueKind::Text),
        ("pivot.range", ValueKind::Range),
        ("pivot.range.min_label", ValueKind::Text),
        ("pivot.range.max_label", ValueKind::Text),
        ("pivot.family.step", ValueKind::Bool),
        ("pivot.family.leap", ValueKind::Bool),
        ("pivot.multiplier.1", ValueKind::Bool),
        ("pivot.multiplier.2", ValueKind::Bool),
        ("pivot.multiplier.4", ValueKind::Bool),
        ("pivot.selected.ratio", ValueKind::Text),
        ("pivot.selected.master", ValueKind::Text),
        ("pivot.selected.target", ValueKind::Text),
        ("pivot.selected.pulse", ValueKind::Text),
        ("pivot.selected.duration", ValueKind::Text),
        ("pivot.selected.loop_a", ValueKind::Text),
        ("pivot.selected.loop_b", ValueKind::Text),
        ("pivot.footer.hint", ValueKind::Text),
        ("pivot.no_tracks.hidden", ValueKind::Bool),
        ("pivot.no_tracks.label", ValueKind::Text),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
    registry.insert(
        EndpointCategory::Parameter,
        "pivot.range",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for (id, kind) in [
        ("pivot.portal.active", ValueKind::Bool),
        ("pivot.portal.hidden", ValueKind::Bool),
        ("pivot.portal.ratio", ValueKind::Text),
        ("pivot.portal.bpm", ValueKind::Text),
        ("pivot.portal.pulse", ValueKind::Text),
        ("pivot.portal.loop", ValueKind::Text),
        ("pivot.portal.stretch", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(kind).with_scope("portal"),
        );
    }
    for (id, kind) in [
        ("pivot.track.hidden", ValueKind::Bool),
        ("pivot.track.title", ValueKind::Text),
        ("pivot.track.artist", ValueKind::Text),
        ("pivot.track.bpm", ValueKind::Text),
        ("pivot.track.key", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(kind).with_scope("track"),
        );
    }
    for id in [
        "pivot.family.step",
        "pivot.family.leap",
        "pivot.multiplier.1",
        "pivot.multiplier.2",
        "pivot.multiplier.4",
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "pivot.portal.select",
        EndpointDesc::new(ValueKind::Trigger).with_scope("portal"),
    );
}

fn insert_quality_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("deck.stream.quality_menu", ValueKind::Bool),
        ("deck.stream.quality", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(kind).with_scope("deck"),
        );
    }
    registry.insert(
        EndpointCategory::Telemetry,
        "deck.stream.quality_hidden",
        EndpointDesc::new(ValueKind::Bool).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Model,
        "deck.stream.variant_active",
        EndpointDesc::new(ValueKind::Bool)
            .with_scope("deck")
            .with_scope("variant"),
    );
    for (id, kind) in [
        ("deck.stream.variant_hidden", ValueKind::Bool),
        ("deck.stream.variant_label", ValueKind::Text),
        ("deck.stream.variant_sub", ValueKind::Text),
    ] {
        registry.insert(
            EndpointCategory::Telemetry,
            id,
            EndpointDesc::new(kind)
                .with_scope("deck")
                .with_scope("variant"),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.toggle_quality_menu",
        EndpointDesc::new(ValueKind::Trigger).with_scope("deck"),
    );
    registry.insert(
        EndpointCategory::Command,
        "deck.stream.select_variant",
        EndpointDesc::new(ValueKind::Trigger)
            .with_scope("deck")
            .with_scope("variant"),
    );
}

pub(crate) fn registry() -> impl EndpointRegistry {
    let mut registry = MockRegistry::default();
    insert_deck_endpoints(&mut registry);
    insert_clock_endpoints(&mut registry);
    insert_pivot_endpoints(&mut registry);
    mock_mixer::insert_endpoints(&mut registry);
    mock_stress::insert_endpoints(&mut registry);
    insert_output_levels(&mut registry);
    insert_engine_endpoints(&mut registry);
    for id in ["player.output.volume", "mock.cells.segmented", "vis.preset"] {
        registry.insert(
            EndpointCategory::Parameter,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    for id in ["vis.next", "vis.previous"] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger),
        );
    }
    insert_library_endpoints(&mut registry);
    insert_menu_endpoints(&mut registry);
    insert_quality_endpoints(&mut registry);
    for id in [
        "gallery.label.knobs",
        "gallery.label.meters",
        "gallery.label.toggles",
        "gallery.label.readouts",
        "gallery.label.chips",
        "gallery.label.transport",
        "gallery.label.regular",
        "gallery.label.text",
        "gallery.label.faders",
        "gallery.label.scalar",
        "mock.track.title",
        "mock.track.artist",
        "mock.bpm",
        "mock.key",
        "mock.remain",
        "gallery.footer.deck",
        "gallery.footer.deck_micro",
        "gallery.footer.global_bar",
        "gallery.footer.telemetry",
        "gallery.footer.layout",
        "gallery.footer.tokens_anatomy",
        "vis.preset_index",
        "vis.preset_name",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Text),
        );
    }
    for id in [
        "gallery.tab.atoms",
        "gallery.tab.buttons",
        "gallery.tab.faders",
        "gallery.tab.modules",
        "gallery.tab.typography",
        "gallery.tab.cells",
        "gallery.tab.sizes",
        "gallery.tab.tokens",
        "gallery.tab.micro",
        "gallery.tab.mixer",
        "gallery.tab.vis",
        "gallery.tab.chrome",
        "gallery.tab.titlebars",
        "gallery.tab.tracklist",
        "gallery.tab.tree",
        "gallery.tab.library2",
        "gallery.tab.stress",
        "gallery.tab.menu",
        "gallery.tab.clock",
        "gallery.tab.pivot",
        "gallery.module.deck",
        "gallery.module.deck_micro",
        "gallery.module.global_bar",
        "gallery.module.telemetry",
        "gallery.module.layout",
        "mock.toggle.on",
        "mock.toggle.off",
        "mock.checkbox.on",
        "mock.checkbox.off",
        "mock.chip.active",
        "mock.chip.inactive",
        "mock.button.play",
        "mock.button.cue",
        "mock.button.sync",
        "vis.badge",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Bool),
        );
    }
    for id in [
        "deck.view.zoom",
        "mock.knob.26",
        "mock.knob.28",
        "mock.knob.34",
        "mock.knob.38",
        "mock.volume",
        "mock.cells.segmented",
        "vis.preset",
    ] {
        registry.insert(
            EndpointCategory::Model,
            id,
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
    insert_tracklist_endpoints(&mut registry);
    registry.insert(
        EndpointCategory::Model,
        "mock.levels",
        EndpointDesc::new(ValueKind::Stereo),
    );
    registry
}

fn insert_tracklist_endpoints(registry: &mut MockRegistry) {
    registry.insert(
        EndpointCategory::Model,
        "gallery.tracklist.preset",
        EndpointDesc::new(ValueKind::Scalar),
    );
    for column in Consts::TRACK_COLUMNS {
        registry.insert(
            EndpointCategory::Model,
            &format!("gallery.tracklist.columns.{}", column.endpoint_name()),
            EndpointDesc::new(ValueKind::Bool),
        );
        registry.insert(
            EndpointCategory::Model,
            &format!("gallery.tracklist.columns.width.{}", column.endpoint_name()),
            EndpointDesc::new(ValueKind::Scalar),
        );
    }
}

fn insert_menu_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("ui.menu.open", ValueKind::Bool),
        ("ui.window.can_open", ValueKind::Bool),
        ("ui.prefs.wave_follow", ValueKind::Bool),
        ("ui.prefs.autogain", ValueKind::Bool),
        ("ui.prefs.mono", ValueKind::Bool),
        ("ui.set.recording", ValueKind::Bool),
        ("ui.set.casting", ValueKind::Bool),
        ("ui.window.count", ValueKind::Text),
        ("ui.modules.title", ValueKind::Text),
        ("ui.modules.count", ValueKind::Text),
        ("ui.layouts.active", ValueKind::Text),
        ("ui.set.record_hint", ValueKind::Text),
        ("ui.set.record_time", ValueKind::Text),
        ("ui.set.cast_hint", ValueKind::Text),
        ("gallery.menu.action", ValueKind::Text),
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
        ("gallery.menu.context", ValueKind::Bool, "row"),
        ("gallery.menu.selected", ValueKind::Bool, "row"),
        ("gallery.menu.track", ValueKind::Text, "row"),
        ("gallery.menu.bpm", ValueKind::Text, "row"),
        ("gallery.menu.energy", ValueKind::Scalar, "row"),
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
        "ui.set.toggle_record",
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
        ("gallery.menu.select", "row"),
        ("gallery.menu.queue", "row"),
    ] {
        registry.insert(
            EndpointCategory::Command,
            id,
            EndpointDesc::new(ValueKind::Trigger).with_scope(scope),
        );
    }
    registry.insert(
        EndpointCategory::Command,
        "gallery.menu.load",
        EndpointDesc::new(ValueKind::Trigger)
            .with_scope("deck")
            .with_scope("row"),
    );
}

fn insert_library_endpoints(registry: &mut MockRegistry) {
    for (id, kind) in [
        ("library.visible_tracks", ValueKind::TrackList),
        ("library.tree", ValueKind::Tree),
        ("library.breadcrumb", ValueKind::Text),
        ("library.query", ValueKind::Text),
        ("library.scope", ValueKind::Scalar),
        ("ui.preset", ValueKind::Text),
    ] {
        registry.insert(EndpointCategory::Model, id, EndpointDesc::new(kind));
    }
}
