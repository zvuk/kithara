use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

/// EQ knob travel in dB: knob `0.0` is `EQ_MIN_DB`, knob `1.0` is `EQ_MAX_DB`.
pub(super) const EQ_MIN_DB: f32 = -24.0;
pub(super) const EQ_MAX_DB: f32 = 6.0;

struct Endpoint {
    category: EndpointCategory,
    id: &'static str,
    value: ValueKind,
    deck_scoped: bool,
}

impl Endpoint {
    fn desc(&self) -> EndpointDesc {
        let desc = EndpointDesc::new(self.value);
        if self.deck_scoped {
            desc.with_scope("deck")
        } else {
            desc
        }
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
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.playing",
        value: ValueKind::Bool,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.focused",
        value: ValueKind::Bool,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.position_secs",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.tempo",
        value: ValueKind::Text,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "deck.playback.remain",
        value: ValueKind::Text,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.toggle_play",
        value: ValueKind::Trigger,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.prev",
        value: ValueKind::Trigger,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.next",
        value: ValueKind::Trigger,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.transport.seek_normalized",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.tempo.rate",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.low",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.mid",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "deck.eq.high",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "deck.view.zoom",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "deck.queue.load",
        value: ValueKind::Trigger,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.drag.over",
        value: ValueKind::Bool,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.drag.track",
        value: ValueKind::Text,
        deck_scoped: false,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "mixer.trim",
        value: ValueKind::Scalar,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "mixer.volume",
        value: ValueKind::Stereo,
        deck_scoped: true,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "mix.crossfader",
        value: ValueKind::Scalar,
        deck_scoped: false,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "library.tracks",
        value: ValueKind::TrackList,
        deck_scoped: false,
    },
    Endpoint {
        category: EndpointCategory::Parameter,
        id: "ui.layout.decks",
        value: ValueKind::Scalar,
        deck_scoped: false,
    },
    Endpoint {
        category: EndpointCategory::Telemetry,
        id: "engine.load",
        value: ValueKind::Scalar,
        deck_scoped: false,
    },
];

#[cfg(test)]
pub(super) fn readable_endpoints() -> impl Iterator<Item = (&'static str, bool)> {
    ENDPOINTS
        .iter()
        .filter(|endpoint| endpoint.category != EndpointCategory::Command)
        .map(|endpoint| (endpoint.id, endpoint.deck_scoped))
}

struct Registration {
    endpoint: &'static Endpoint,
    desc: EndpointDesc,
}

/// Registry over the static endpoint table; built once at studio compile time.
pub(super) struct StudioRegistry {
    endpoints: Vec<Registration>,
}

impl StudioRegistry {
    pub(super) fn new() -> Self {
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
