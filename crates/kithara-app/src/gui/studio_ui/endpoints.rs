use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

/// EQ knob travel in dB: knob `0.0` is `EQ_MIN_DB`, knob `1.0` is `EQ_MAX_DB`.
pub(in crate::gui) const EQ_MIN_DB: f32 = -24.0;
pub(in crate::gui) const EQ_MAX_DB: f32 = 6.0;

struct Endpoint {
    category: EndpointCategory,
    id: &'static str,
    value: ValueKind,
    scopes: &'static [&'static str],
}

impl Endpoint {
    const DECK: &[&str] = &["deck"];
    const GLOBAL: &[&str] = &[];
    const VARIANT: &[&str] = &["deck", "variant"];

    fn desc(&self) -> EndpointDesc {
        self.scopes
            .iter()
            .fold(EndpointDesc::new(self.value), |desc, scope| {
                desc.with_scope(scope)
            })
    }
}

/// Endpoint table the studio documents compile against. Categories follow the
/// binding direction: `Command` for triggers, `Parameter` for read/write
/// scalars, `Telemetry` for engine state, `Model` for host-owned UI state.
static ENDPOINTS: &[Endpoint] = &[
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
        value: ValueKind::Trigger,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.drag.over",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.drag.track",
        value: ValueKind::Text,
        scopes: Endpoint::GLOBAL,
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
        id: "mix.crossfader",
        value: ValueKind::Scalar,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "library.tracks",
        value: ValueKind::TrackList,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "ui.layout.decks",
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
        category: EndpointCategory::Model,
        id: "deck.stream.quality",
        value: ValueKind::Text,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.stream.quality_menu",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.stream.quality_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::DECK,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.stream.toggle_quality_menu",
        value: ValueKind::Trigger,
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

struct Registration {
    endpoint: &'static Endpoint,
    desc: EndpointDesc,
}

/// Registry over the static endpoint table; built once at studio compile time.
pub(super) struct StudioRegistry {
    endpoints: Vec<Registration>,
}

impl Default for StudioRegistry {
    fn default() -> Self {
        let endpoints = ENDPOINTS
            .iter()
            .map(|endpoint| Registration {
                endpoint,
                desc: endpoint.desc(),
            })
            .collect();
        Self { endpoints }
    }
}

impl EndpointRegistry for StudioRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints
            .iter()
            .find(|entry| entry.endpoint.category == category && entry.endpoint.id == id.0)
            .map(|entry| &entry.desc)
    }
}
