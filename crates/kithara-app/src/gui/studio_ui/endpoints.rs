use kithara_ui::{
    ids::EndpointId,
    registry::{EndpointCategory, EndpointDesc, EndpointRegistry, ValueKind},
};

/// Tempo range presets, indexed by the `range` segmented control.
pub(super) const TS_RANGES: [u8; 4] = [8, 16, 50, 100];

/// EQ knob travel in dB: knob `0.0` is `EQ_MIN_DB`, knob `1.0` is `EQ_MAX_DB`.
pub(super) const EQ_MIN_DB: f32 = -24.0;
pub(super) const EQ_MAX_DB: f32 = 6.0;

/// Endpoint table the studio documents compile against. Categories follow the
/// binding direction: `Command` for triggers, `Parameter` for read/write
/// scalars, `Telemetry` for engine state, `Model` for host-owned UI state.
const ENDPOINTS: &[(EndpointCategory, &str, ValueKind, bool)] = &[
    (
        EndpointCategory::Telemetry,
        "deck.playback.waveform",
        ValueKind::Waveform,
        true,
    ),
    (
        EndpointCategory::Telemetry,
        "deck.playback.playing",
        ValueKind::Bool,
        true,
    ),
    (
        EndpointCategory::Telemetry,
        "deck.playback.position_secs",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Telemetry,
        "deck.playback.tempo",
        ValueKind::Text,
        true,
    ),
    (
        EndpointCategory::Telemetry,
        "deck.ts.ratio",
        ValueKind::Text,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.transport.toggle_play",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.transport.prev",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.transport.next",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.transport.seek_normalized",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.ts.reset",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.ts.toggle_keylock",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "deck.ts.tempo",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Model,
        "deck.ts.range_index",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Model,
        "deck.ts.keylock",
        ValueKind::Bool,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "deck.eq.low",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "deck.eq.mid",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "deck.eq.high",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Model,
        "deck.view.zoom",
        ValueKind::Scalar,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "mixer.trim",
        ValueKind::Scalar,
        true,
    ),
    (EndpointCategory::Model, "mixer.muted", ValueKind::Bool, true),
    (
        EndpointCategory::Command,
        "mixer.toggle_mute",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Command,
        "deck.load_selected",
        ValueKind::Trigger,
        true,
    ),
    (
        EndpointCategory::Parameter,
        "mix.crossfader",
        ValueKind::Scalar,
        false,
    ),
    (
        EndpointCategory::Parameter,
        "mix.group_master",
        ValueKind::Scalar,
        false,
    ),
    (
        EndpointCategory::Model,
        "library.tracks",
        ValueKind::TrackList,
        false,
    ),
];

/// Registry over the static endpoint table; built once at studio compile time.
pub(super) struct StudioRegistry {
    endpoints: Vec<((EndpointCategory, EndpointId), EndpointDesc)>,
}

impl StudioRegistry {
    pub(super) fn new() -> Self {
        let endpoints = ENDPOINTS
            .iter()
            .map(|(category, id, kind, deck_scoped)| {
                let desc = if *deck_scoped {
                    EndpointDesc::new(*kind).with_scope("deck")
                } else {
                    EndpointDesc::new(*kind)
                };
                ((*category, EndpointId((*id).to_owned())), desc)
            })
            .collect();
        Self { endpoints }
    }
}

impl EndpointRegistry for StudioRegistry {
    fn endpoint(&self, category: EndpointCategory, id: &EndpointId) -> Option<&EndpointDesc> {
        self.endpoints
            .iter()
            .find(|((have_category, have_id), _)| *have_category == category && have_id == id)
            .map(|(_, desc)| desc)
    }
}
