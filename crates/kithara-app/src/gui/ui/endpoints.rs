use kithara::ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

struct Endpoint {
    scopes: &'static [&'static str],
    id: &'static str,
    category: EndpointCategory,
    value: ValueKind,
}

impl Endpoint {
    const DECK: &[&str] = &["deck"];
    const EQ_MODE: &[&str] = &["deck", "bands"];
    const GLOBAL: &[&str] = &[];
    const LAYOUT: &[&str] = &["layout"];
    const MODULE: &[&str] = &["module"];
    const SOURCE: &[&str] = &["source"];
    const COLUMN: &[&str] = &["source", "column"];
    const VARIANT: &[&str] = &["deck", "variant"];
    const WINDOW: &[&str] = &["window"];

    fn desc(&self) -> EndpointDesc {
        self.scopes
            .iter()
            .fold(EndpointDesc::new(self.value), |desc, scope| {
                desc.with_scope(scope)
            })
    }
}

/// Endpoint table the app documents compile against. Categories follow the
/// binding direction: `Command` for triggers, `Parameter` for read/write
/// scalars, `Telemetry` for engine state, `Model` for host-owned UI state.
static ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.track.artwork",
        value: ValueKind::Image,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "source.column.width",
        value: ValueKind::Scalar,
        scopes: Endpoint::COLUMN,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.waveform",
        value: ValueKind::Waveform,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.playing",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.focused",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.position_secs",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.duration_secs",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.track.title",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.track.source_kind",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.tempo",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.bpm",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.remain",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.track.title",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.toggle_play",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.prev",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.next",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.seek_normalized",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.tempo.rate",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.tempo.reset",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.low",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.mid",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.high",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.low_mid",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.high_mid",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.eq.bands",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.eq.selected",
        value: ValueKind::Bool,
        scopes: Endpoint::EQ_MODE,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.eq.mode",
        value: ValueKind::Trigger,
        scopes: Endpoint::EQ_MODE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.view.zoom",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.view.zoom_in",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.view.zoom_out",
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.queue.load",
        value: ValueKind::Record,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "mixer.trim",
        value: ValueKind::Scalar,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "mixer.volume",
        value: ValueKind::Stereo,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "mixer.muted",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "mix.crossfader",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "player.output.levels",
        value: ValueKind::Stereo,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "player.output.volume",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "library.tree",
        value: ValueKind::Tree,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "library.select",
        value: ValueKind::Index,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "library.toggle",
        value: ValueKind::Index,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "library.page",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "library.add_folder.hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "source.select",
        value: ValueKind::Index,
        scopes: Endpoint::SOURCE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "source.rows",
        value: ValueKind::Table,
        scopes: Endpoint::SOURCE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "source.status",
        value: ValueKind::Text,
        scopes: Endpoint::SOURCE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "tempo.map",
        value: ValueKind::PortalMap,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "vis.preset",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "vis.time",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "engine.load",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "broadcast.on_air",
        value: ValueKind::Bool,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "broadcast.url",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "broadcast.hint",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "broadcast.hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "broadcast.toggle",
        value: ValueKind::Trigger,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.app.version",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.window.toggle_full_screen",
        value: ValueKind::Trigger,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.library.add_folder",
        value: ValueKind::Trigger,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.active",
        value: ValueKind::Bool,
        scopes: Endpoint::WINDOW,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.close_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::WINDOW,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.chrome_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::WINDOW,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.title",
        value: ValueKind::Text,
        scopes: Endpoint::WINDOW,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.caption",
        value: ValueKind::Text,
        scopes: Endpoint::WINDOW,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.window.count",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.modules.count",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.module.on",
        value: ValueKind::Bool,
        scopes: Endpoint::MODULE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.module.hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::MODULE,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.module.toggle",
        value: ValueKind::Trigger,
        scopes: Endpoint::MODULE,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.layouts.active",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.layout.selected",
        value: ValueKind::Bool,
        scopes: Endpoint::LAYOUT,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.layout.apply",
        value: ValueKind::Trigger,
        scopes: Endpoint::LAYOUT,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.stream.quality",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.stream.quality_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.stream.variant_active",
        value: ValueKind::Bool,
        scopes: Endpoint::VARIANT,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.stream.variant_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::VARIANT,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.stream.variant_label",
        value: ValueKind::Text,
        scopes: Endpoint::VARIANT,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.stream.variant_sub",
        value: ValueKind::Text,
        scopes: Endpoint::VARIANT,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.stream.select_variant",
        value: ValueKind::Trigger,
        scopes: Endpoint::VARIANT,
    },
];

#[cfg(test)]
pub(in crate::gui) fn readable_endpoints()
-> impl Iterator<Item = (&'static str, &'static [&'static str])> {
    ENDPOINTS
        .iter()
        .filter(|endpoint| endpoint.category != EndpointCategory::Command)
        .map(|endpoint| (endpoint.id, endpoint.scopes))
}

#[cfg(all(test, feature = "masonry"))]
pub(in crate::gui) fn readable_kind(id: &str) -> Option<ValueKind> {
    ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.category != EndpointCategory::Command && endpoint.id == id)
        .map(|endpoint| endpoint.value)
}

struct Registration {
    category: EndpointCategory,
    id: EndpointId,
    desc: EndpointDesc,
}

/// Registry over the static endpoint table; built once at compile time.
pub(crate) struct Registry {
    endpoints: Vec<Registration>,
}

impl Default for Registry {
    fn default() -> Self {
        let endpoints = ENDPOINTS
            .iter()
            .map(|endpoint| Registration {
                category: endpoint.category,
                id: EndpointId(endpoint.id.to_owned()),
                desc: endpoint.desc(),
            })
            .collect();
        Self { endpoints }
    }
}

impl Registry {
    pub(in crate::gui) fn with_endpoints(
        mut self,
        endpoints: impl IntoIterator<Item = (EndpointCategory, EndpointId, EndpointDesc)>,
    ) -> Self {
        self.endpoints.extend(
            endpoints
                .into_iter()
                .map(|(category, id, desc)| Registration { category, id, desc }),
        );
        self
    }
}

impl EndpointRegistry for Registry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints
            .iter()
            .find(|entry| entry.category == category && entry.id == *id)
            .map(|entry| &entry.desc)
    }
}
