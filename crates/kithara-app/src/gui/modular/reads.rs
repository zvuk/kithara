use kithara_ui::render::{ReadValue, Reads, TrackRow, WaveBucket, WaveformView};
use num_traits::ToPrimitive;

use crate::state::UiState;

struct Consts;

impl Consts {
    const CONFIGURED_SOURCE: &'static str = "configured source";
    const EM_DASH: &'static str = "\u{2014}";
    const HLS_SOURCE: &'static str = "HLS stream";
    const NETWORK_SOURCE: &'static str = "network stream";
    const LOCAL_SOURCE: &'static str = "local file";
    const NO_SOURCE: &'static str = "no source";
}

pub(super) struct UiReads<'state> {
    state: &'state UiState,
    query: &'state str,
    preset: &'state str,
    buckets: Vec<WaveBucket>,
    tracks: Vec<TrackRow<'state>>,
}

impl<'state> UiReads<'state> {
    pub(super) fn new(
        state: &'state UiState,
        query: &'state str,
        preset: &'state str,
        selected: Option<usize>,
    ) -> Self {
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
            .enumerate()
            .map(|(index, track)| TrackRow {
                title: track.name.as_str(),
                artist: None,
                time: None,
                search: track.url.as_deref(),
                current: state.current_track_index == Some(index),
                selected: selected == Some(index),
            })
            .collect();
        Self {
            state,
            query,
            preset,
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
            "deck.playback.remaining_secs" => Some(ReadValue::Scalar(
                (self.state.duration - self.state.position).max(0.0),
            )),
            "deck.playback.position_secs" => Some(ReadValue::Scalar(self.state.position)),
            "deck.playback.duration_secs" => Some(ReadValue::Scalar(self.state.duration)),
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
            "deck.track.source_kind" => Some(ReadValue::Text(source_kind(self.state))),
            "player.output.volume" => Some(ReadValue::Scalar(f64::from(self.state.volume))),
            "library.visible_tracks" => Some(ReadValue::TrackList(&self.tracks)),
            "library.query" => Some(ReadValue::Text(self.query)),
            "ui.preset" => Some(ReadValue::Text(self.preset)),
            _ => None,
        }
    }
}

fn position_normalized(ui: &UiState) -> f64 {
    if ui.duration > 0.0 {
        (ui.position / ui.duration).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn source_kind(state: &UiState) -> &'static str {
    if !state.variant_label.is_empty() {
        return Consts::HLS_SOURCE;
    }
    let entry = state
        .current_track_index
        .and_then(|index| state.tracks.get(index));
    match entry.and_then(|track| track.url.as_deref()) {
        Some(url) if url.contains(".m3u8") => Consts::HLS_SOURCE,
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            Consts::NETWORK_SOURCE
        }
        Some(_) => Consts::LOCAL_SOURCE,
        None if entry.is_some() => Consts::CONFIGURED_SOURCE,
        _ if state.track_name.is_empty() => Consts::EM_DASH,
        _ => Consts::NO_SOURCE,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use kithara_ui::render::{ReadValue, Reads};

    use super::UiReads;
    use crate::state::UiState;

    fn reads(ui: &UiState) -> UiReads<'_> {
        UiReads::new(ui, "query", "preset", None)
    }

    #[kithara::test]
    fn resolves_playing_for_deck_a() {
        for playing in [false, true] {
            let mut ui = UiState::empty();
            ui.playing = playing;
            let reads = reads(&ui);
            let Some(ReadValue::Bool(value)) = reads.get("deck.playback.playing") else {
                panic!("playing endpoint must resolve to bool");
            };
            assert_eq!(value, playing);
        }
    }

    #[kithara::test]
    fn normalized_position_is_zero_without_duration() {
        let mut ui = UiState::empty();
        ui.position = 12.0;
        let reads = reads(&ui);

        let Some(ReadValue::Scalar(value)) = reads.get("deck.playback.position_normalized") else {
            panic!("position endpoint must resolve to scalar");
        };
        assert!(value.abs() < f64::EPSILON);
    }

    #[kithara::test]
    fn normalized_position_is_clamped() {
        for (position, expected) in [(-2.0, 0.0), (25.0, 0.25), (120.0, 1.0)] {
            let mut ui = UiState::empty();
            ui.duration = 100.0;
            ui.position = position;
            let reads = reads(&ui);
            let Some(ReadValue::Scalar(value)) = reads.get("deck.playback.position_normalized")
            else {
                panic!("position endpoint must resolve to scalar");
            };
            assert!((value - expected).abs() < f64::EPSILON);
        }
    }

    #[kithara::test]
    fn resolves_new_renderer_endpoints() {
        let mut ui = UiState::empty();
        ui.position = 12.0;
        ui.duration = 30.0;
        let reads = UiReads::new(&ui, "needle", "player.klayout.ron", None);

        assert_eq!(
            reads.get("deck.playback.position_secs"),
            Some(ReadValue::Scalar(12.0))
        );
        assert_eq!(
            reads.get("deck.playback.duration_secs"),
            Some(ReadValue::Scalar(30.0))
        );
        assert_eq!(
            reads.get("deck.playback.remaining_secs"),
            Some(ReadValue::Scalar(18.0))
        );
        assert_eq!(reads.get("library.query"), Some(ReadValue::Text("needle")));
        assert_eq!(
            reads.get("ui.preset"),
            Some(ReadValue::Text("player.klayout.ron"))
        );
    }
}
