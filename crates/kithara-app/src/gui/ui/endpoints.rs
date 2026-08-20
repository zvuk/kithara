use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

/// EQ knob travel in dB: knob `0.0` is `EQ_MIN_DB`, knob `0.5` is unity, knob
/// `1.0` is `EQ_MAX_DB`.
pub(in crate::gui) const EQ_MIN_DB: f32 = -24.0;
pub(in crate::gui) const EQ_MAX_DB: f32 = 6.0;

pub(in crate::gui) fn db_from_knob(knob: f32) -> f32 {
    let offset = knob.clamp(0.0, 1.0) - 0.5;
    2.0 * offset * half_span(offset)
}

pub(in crate::gui) fn knob_from_db(db: f32) -> f32 {
    let db = db.clamp(EQ_MIN_DB, EQ_MAX_DB);
    0.5 + db / (2.0 * half_span(db))
}

fn half_span(side: f32) -> f32 {
    if side < 0.0 { -EQ_MIN_DB } else { EQ_MAX_DB }
}

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
    const GROUP: &[&str] = &["group"];
    const LAYOUT: &[&str] = &["layout"];
    const MODULE: &[&str] = &["module"];
    const WINDOW: &[&str] = &["window"];
    const VARIANT: &[&str] = &["deck", "variant"];

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
        id: "deck.eq.menu_open",
        value: ValueKind::Bool,
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
        id: "deck.eq.menu",
        value: ValueKind::Trigger,
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
        category: EndpointCategory::Model,
        id: "ui.menu.open",
        value: ValueKind::Bool,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.menu.toggle",
        value: ValueKind::Trigger,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.menu.close",
        value: ValueKind::Trigger,
        scopes: Endpoint::GLOBAL,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.menu.group_open",
        value: ValueKind::Bool,
        scopes: Endpoint::GROUP,
    },
    Endpoint {
        category: EndpointCategory::Model,
        id: "ui.menu.group_hidden",
        value: ValueKind::Bool,
        scopes: Endpoint::GROUP,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.menu.toggle_group",
        value: ValueKind::Trigger,
        scopes: Endpoint::GROUP,
    },
    Endpoint {
        category: EndpointCategory::Command,
        id: "ui.window.toggle_full_screen",
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

/// Registry over the static endpoint table; built once at compile time.
pub(super) struct Registry {
    endpoints: Vec<Registration>,
}

impl Default for Registry {
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

impl EndpointRegistry for Registry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints
            .iter()
            .find(|entry| entry.endpoint.category == category && entry.endpoint.id == id.0)
            .map(|entry| &entry.desc)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const STEPS: usize = 256;

    #[kithara::test]
    fn unity_gain_sits_at_the_middle_of_the_knob_travel() {
        assert_eq!(knob_from_db(0.0), 0.5);
        assert_eq!(db_from_knob(0.5), 0.0);
        assert_eq!(db_from_knob(0.0), EQ_MIN_DB);
        assert_eq!(db_from_knob(1.0), EQ_MAX_DB);
    }

    #[kithara::test]
    fn reading_back_a_written_gain_lands_on_the_same_knob_position() {
        for step in 0..=STEPS {
            let knob = step as f32 / STEPS as f32;
            let round_trip = knob_from_db(db_from_knob(knob));
            assert!(
                (round_trip - knob).abs() < 1e-6,
                "knob {knob} came back as {round_trip}"
            );
        }
    }
}
