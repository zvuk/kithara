use std::collections::BTreeMap;

use kithara_ui::{
    compile::CompiledUi,
    expand::Binding,
    ids::InternId,
    render::{ReadValue, Reads, TrackRow, WaveBucket, WaveformView},
};
use num_traits::ToPrimitive;

use crate::state::UiState;

pub(super) struct UiReads<'state> {
    state: &'state UiState,
    buckets: Vec<WaveBucket>,
    tracks: Vec<TrackRow<'state>>,
}

impl<'state> UiReads<'state> {
    pub(super) fn new(state: &'state UiState) -> Self {
        let buckets = state
            .analysis
            .as_ref()
            .and_then(|analysis| analysis.waveform())
            .map(|waveform| {
                waveform
                    .buckets()
                    .iter()
                    .map(|bucket| WaveBucket {
                        low: bucket.low(),
                        mid: bucket.mid(),
                        high: bucket.high(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tracks = state
            .tracks
            .iter()
            .map(|track| TrackRow {
                title: track.name.as_str(),
                artist: None,
                time: None,
            })
            .collect();
        Self {
            state,
            buckets,
            tracks,
        }
    }
}

impl Reads for UiReads<'_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        match endpoint {
            "deck.playback.playing" => Some(ReadValue::Bool(self.state.playing)),
            "deck.playback.position_normalized" => {
                Some(ReadValue::Scalar(position_normalized(self.state)))
            }
            "deck.playback.waveform" => {
                let analysis = self.state.analysis.as_ref()?;
                let bpm = analysis
                    .beat()
                    .map(kithara::audio::BeatGrid::bpm)
                    .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
                    .and_then(|bpm| bpm.to_f32());
                Some(ReadValue::Waveform(WaveformView {
                    buckets: &self.buckets,
                    beats: &self.state.beat_marks,
                    downbeats: &self.state.downbeat_marks,
                    bpm,
                }))
            }
            "deck.track.title" => Some(ReadValue::Text(self.state.track_name.as_str())),
            "player.output.volume" => Some(ReadValue::Scalar(f64::from(self.state.volume))),
            "library.visible_tracks" => Some(ReadValue::TrackList(&self.tracks)),
            _ => None,
        }
    }
}

pub(super) fn resolve<'a>(
    reads: &'a dyn Reads,
    binding: &Binding,
    ui: &CompiledUi,
) -> Option<ReadValue<'a>> {
    match binding {
        Binding::Telemetry { id, with } if deck_is_a(with, ui) => reads.get(ui.resolve(*id)),
        Binding::Parameter { id, .. } | Binding::Model { id, .. } => reads.get(ui.resolve(*id)),
        _ => None,
    }
}

pub(super) fn position_normalized(ui: &UiState) -> f64 {
    if ui.duration > 0.0 {
        (ui.position / ui.duration).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

pub(super) fn deck_is_a(scope: &BTreeMap<InternId, InternId>, ui: &CompiledUi) -> bool {
    scope
        .iter()
        .any(|(key, value)| ui.resolve(*key) == "deck" && ui.resolve(*value) == "a")
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::{
        compile::{CompiledNode, CompiledUi, compile},
        expand::{Binding, ExpandedNode},
        render::ReadValue,
        source::{MemResolver, UiConfig},
    };

    use super::{UiReads, resolve};
    use crate::{gui::modular::endpoints, state::UiState};

    fn compile_read(kind: &str, read: &str) -> (CompiledUi, Binding) {
        let mut resolver = MemResolver::default();
        resolver.insert(
            "read.klayout.ron",
            r#"(schema: "kithara.layout", version: 1, id: "read",
                root: Module(instance: "deck-a", source: "read.kmodule.ron"))"#,
        );
        resolver.insert(
            "read.kmodule.ron",
            &format!(
                r#"(schema: "kithara.module", version: 1, id: "read",
                    root: Control(id: "control", kind: "{kind}", read: {read}))"#
            ),
        );
        let ui = compile(
            "read.klayout.ron",
            &resolver,
            &endpoints::catalog(),
            &endpoints::registry(),
            &UiConfig::default(),
        )
        .unwrap_or_else(|error| panic!("read fixture must compile: {error}"));
        let binding = {
            let CompiledNode::Module { root, .. } = &ui.root else {
                panic!("read fixture must compile to a module");
            };
            let ExpandedNode::Control {
                read: Some(binding),
                ..
            } = &**root
            else {
                panic!("read fixture must contain a read binding");
            };
            binding.clone()
        };
        (ui, binding)
    }

    fn telemetry(id: &str, deck: &str, kind: &str) -> (CompiledUi, Binding) {
        compile_read(
            kind,
            &format!(r#"Telemetry(id: "{id}", with: {{ "deck": "{deck}" }})"#),
        )
    }

    #[kithara::test]
    fn resolves_playing_for_deck_a() {
        let (compiled, binding) = telemetry("deck.playback.playing", "a", "button");
        for playing in [false, true] {
            let mut ui = UiState::empty();
            ui.playing = playing;
            let reads = UiReads::new(&ui);
            let Some(ReadValue::Bool(value)) = resolve(&reads, &binding, &compiled) else {
                panic!("playing binding must resolve to bool");
            };
            assert_eq!(value, playing);
        }
    }

    #[kithara::test]
    fn normalized_position_is_zero_without_duration() {
        let (compiled, binding) =
            telemetry("deck.playback.position_normalized", "a", "telemetry.scalar");
        let mut ui = UiState::empty();
        ui.position = 12.0;
        let reads = UiReads::new(&ui);

        let Some(ReadValue::Scalar(value)) = resolve(&reads, &binding, &compiled) else {
            panic!("position binding must resolve to scalar");
        };
        assert!(value.abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn normalized_position_is_clamped() {
        let (compiled, binding) =
            telemetry("deck.playback.position_normalized", "a", "telemetry.scalar");
        for (position, expected) in [(-2.0, 0.0), (25.0, 0.25), (120.0, 1.0)] {
            let mut ui = UiState::empty();
            ui.duration = 100.0;
            ui.position = position;
            let reads = UiReads::new(&ui);
            let Some(ReadValue::Scalar(value)) = resolve(&reads, &binding, &compiled) else {
                panic!("position binding must resolve to scalar");
            };
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[kithara::test]
    fn resolves_volume_without_deck_scope() {
        let (compiled, binding) = compile_read(
            "fader.horizontal",
            r#"Parameter(id: "player.output.volume")"#,
        );
        let mut ui = UiState::empty();
        ui.volume = 0.37;
        let reads = UiReads::new(&ui);

        let Some(ReadValue::Scalar(value)) = resolve(&reads, &binding, &compiled) else {
            panic!("volume binding must resolve to scalar");
        };
        assert!((value - f64::from(ui.volume)).abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn rejects_unknown_deck_scope() {
        let ui = UiState::empty();
        let reads = UiReads::new(&ui);
        let (compiled, binding) = telemetry("deck.playback.playing", "b", "button");

        assert!(resolve(&reads, &binding, &compiled).is_none());
    }
}
