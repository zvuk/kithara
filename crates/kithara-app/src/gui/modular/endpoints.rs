use std::{collections::BTreeMap, sync::LazyLock};

use kithara_ui::{
    ids::{ControlKind, EndpointId},
    registry::{
        ControlCatalog, ControlKindDesc, EndpointCategory, EndpointDesc, EndpointRegistry,
        PropKind, ValueKind,
    },
    size::{Dim, SizeSpec},
};

use crate::gui::tokens::{deck, global_bar, track_list, transport, volume, waveform};

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
    fn kind(&self, kind: &str) -> Option<&ControlKindDesc> {
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

fn build_catalog() -> AppCatalog {
    let mut catalog = AppCatalog::default();
    catalog.insert(
        "deck.header",
        ControlKindDesc::new(Some(ValueKind::Waveform), None)
            .with_prop("badge", PropKind::Text)
            .with_size(SizeSpec::new(Dim::Fill, Dim::Fixed(deck::HEADER_HEIGHT))),
    );
    catalog.insert(
        "deck.summary",
        ControlKindDesc::new(Some(ValueKind::Text), None)
            .with_prop("style", PropKind::Text)
            .with_size(SizeSpec::new(
                Dim::Range {
                    min: deck::SUMMARY_MIN_WIDTH,
                    max: None,
                },
                Dim::Fixed(deck::SUMMARY_HEIGHT),
            )),
    );
    catalog.insert(
        "global.brand",
        ControlKindDesc::new(None, None).with_size(SizeSpec::new(
            Dim::Fixed(global_bar::BRAND_WIDTH),
            Dim::Fixed(global_bar::HEIGHT),
        )),
    );
    catalog.insert(
        "global.spacer",
        ControlKindDesc::new(None, None)
            .with_size(SizeSpec::new(Dim::Fill, Dim::Fixed(global_bar::HEIGHT))),
    );
    catalog.insert(
        "preset.selector",
        ControlKindDesc::new(None, None).with_size(SizeSpec::new(
            Dim::Fixed(global_bar::SELECTOR_WIDTH),
            Dim::Fixed(global_bar::HEIGHT),
        )),
    );
    catalog.insert(
        "view.settings",
        ControlKindDesc::new(None, None).with_size(SizeSpec::new(
            Dim::Fixed(global_bar::SETTINGS_WIDTH),
            Dim::Fixed(global_bar::HEIGHT),
        )),
    );
    catalog.insert(
        "text",
        ControlKindDesc::new(Some(ValueKind::Text), None)
            .with_prop("style", PropKind::Text)
            .with_size(SizeSpec::new(Dim::Fill, Dim::Fixed(18.0))),
    );
    catalog.insert(
        "button",
        ControlKindDesc::new(Some(ValueKind::Bool), Some(ValueKind::Trigger))
            .with_prop("label", PropKind::Text)
            .with_prop("active-label", PropKind::Text)
            .with_prop("style", PropKind::Text)
            .with_size(SizeSpec::new(
                Dim::Range {
                    min: transport::BUTTON_MIN_WIDTH,
                    max: None,
                },
                Dim::Fixed(transport::BUTTON_HEIGHT),
            )),
    );
    catalog.insert(
        "telemetry.bpm",
        ControlKindDesc::new(Some(ValueKind::Waveform), None)
            .with_prop("fallback", PropKind::Text)
            .with_size(SizeSpec::new(
                Dim::Fixed(deck::BPM_CELL_WIDTH),
                Dim::Fixed(global_bar::HEIGHT),
            )),
    );
    catalog.insert(
        "telemetry.time",
        ControlKindDesc::new(Some(ValueKind::Scalar), None).with_size(SizeSpec::new(
            Dim::Fixed(transport::TIME_WIDTH),
            Dim::Fixed(transport::BUTTON_HEIGHT),
        )),
    );
    catalog.insert(
        "telemetry.scalar",
        ControlKindDesc::new(Some(ValueKind::Scalar), None)
            .with_prop("format", PropKind::Text)
            .with_size(SizeSpec::new(Dim::Fixed(64.0), Dim::Fixed(18.0))),
    );
    catalog.insert(
        "fader.horizontal",
        ControlKindDesc::new(Some(ValueKind::Scalar), Some(ValueKind::Scalar))
            .with_prop("style", PropKind::Text)
            .with_size(SizeSpec::new(
                Dim::Range {
                    min: volume::MIN_WIDTH,
                    max: None,
                },
                Dim::Fixed(volume::CONTROL_HEIGHT),
            )),
    );
    catalog.insert(
        "waveform.mini",
        ControlKindDesc::new(Some(ValueKind::Waveform), Some(ValueKind::Scalar))
            .with_prop("style", PropKind::Text)
            .with_size(SizeSpec::new(
                Dim::Fill,
                Dim::Range {
                    min: waveform::HERO_HEIGHT,
                    max: None,
                },
            )),
    );
    catalog.insert(
        "track_list",
        ControlKindDesc::new(Some(ValueKind::TrackList), None).with_size(SizeSpec::new(
            Dim::Range {
                min: track_list::MIN_WIDTH,
                max: None,
            },
            Dim::Range {
                min: track_list::MIN_HEIGHT,
                max: None,
            },
        )),
    );
    catalog
}

static RENDER_CATALOG: LazyLock<AppCatalog> = LazyLock::new(build_catalog);

pub(crate) fn catalog() -> AppCatalog {
    build_catalog()
}

pub(crate) fn render_catalog() -> &'static AppCatalog {
    &RENDER_CATALOG
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
    use kithara_ui::{builtin, compile::compile, source::UiConfig};

    use super::*;

    #[kithara::test]
    fn builtin_presets_compile_against_app_registry() {
        for preset in [builtin::MICRO_PRESET, builtin::PLAYER_PRESET] {
            compile(
                preset,
                &builtin::resolver(),
                &catalog(),
                &registry(),
                &UiConfig::default(),
            )
            .unwrap_or_else(|error| panic!("{preset}: {error}"));
        }
    }

    #[kithara::test]
    fn micro_window_size_is_derived_from_catalog() {
        let ui = compile(
            builtin::MICRO_PRESET,
            &builtin::resolver(),
            &catalog(),
            &registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("micro preset must compile: {error}"));

        assert!(ui.size.w.min() > 0.0, "width min: {}", ui.size.w.min());
        assert!(ui.size.h.min() > 0.0, "height min: {}", ui.size.h.min());
    }

    #[kithara::test]
    fn player_window_size_is_derived_from_catalog() {
        let ui = compile(
            builtin::PLAYER_PRESET,
            &builtin::resolver(),
            &catalog(),
            &registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("player preset must compile: {error}"));

        assert!(ui.size.w.min() > 0.0, "width min: {}", ui.size.w.min());
        assert!(ui.size.h.min() > 0.0, "height min: {}", ui.size.h.min());
    }

    #[kithara::test]
    fn visual_controls_declare_intrinsic_sizes() {
        let catalog = catalog();
        for kind in [
            "deck.header",
            "deck.summary",
            "global.brand",
            "preset.selector",
            "telemetry.time",
        ] {
            let description = catalog
                .kind(kind)
                .unwrap_or_else(|| panic!("{kind} must be registered"));
            assert!(description.size.w.min() > 0.0 || description.size.w == Dim::Fill);
            assert!(description.size.h.min() > 0.0, "{kind} height");
        }

        let waveform = catalog
            .kind("waveform.mini")
            .unwrap_or_else(|| panic!("waveform.mini must be registered"));
        assert!(waveform.size.h.min() >= waveform::HERO_HEIGHT);
    }
}
