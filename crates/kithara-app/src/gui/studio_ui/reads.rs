use kithara_ui::render::{ReadValue, Reads, TrackRow, WaveformView};
use num_traits::cast::AsPrimitive;

use super::{
    cache::{DeckCache, head},
    endpoints::{EQ_MAX_DB, EQ_MIN_DB},
    scope::deck_index,
};
use crate::gui::{
    app::Kithara,
    deck::{DeckUi, TEMPO_RANGE},
};

/// Frame-local read adapter: borrows the app state and the studio cache and
/// answers the renderer's canonical scoped endpoint keys.
pub(super) struct StudioReads<'a> {
    state: &'a Kithara,
    rows: Vec<TrackRow<'a>>,
}

impl<'a> StudioReads<'a> {
    pub(super) fn new(state: &'a Kithara) -> Self {
        let cache = &state.studio.cache;
        let rows = state
            .catalog
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| TrackRow {
                title: &entry.name,
                artist: Some(entry.url.as_str()),
                time: None,
                search: None,
                deck: cache
                    .deck_marks
                    .get(index)
                    .map(String::as_str)
                    .filter(|marks| !marks.is_empty()),
                bpm: None,
                key: None,
                energy: None,
                transition: None,
                current: false,
                selected: state.selected_track == Some(index),
            })
            .collect();
        Self { state, rows }
    }

    fn deck(&self, index: usize) -> Option<(&'a DeckUi, &'a DeckCache)> {
        let id = self.state.session.decks().get(index)?.id;
        let deck = self.state.decks.get(id)?;
        let cache = self.state.studio.cache.deck(index)?;
        Some((deck, cache))
    }

    fn deck_value(&self, base: &str, index: usize) -> Option<ReadValue<'a>> {
        let (deck, cache) = self.deck(index)?;
        let ui = &deck.ui;
        let ts = deck.view.timestretch;
        let head = head(ui);
        let duration = ui.duration.max(0.0);
        let value = match base {
            "deck.playback.waveform" => ReadValue::Waveform(WaveformView {
                buckets: &cache.wave,
                beats: &ui.beat_marks,
                downbeats: &ui.downbeat_marks,
                bpm: analysis_bpm(ui),
                r#loop: None,
                cues: &[],
            }),
            "deck.playback.playing" => ReadValue::Bool(ui.playing),
            "deck.playback.position_secs" => ReadValue::Scalar(head.max(0.0)),
            "deck.playback.duration_secs" => ReadValue::Scalar(duration),
            "deck.playback.remaining_secs" => ReadValue::Scalar((duration - head).max(0.0)),
            "deck.playback.position_normalized" => {
                let normalized = if duration > 0.0 { head / duration } else { 0.0 };
                ReadValue::Scalar(normalized.clamp(0.0, 1.0))
            }
            "deck.playback.tempo" => ReadValue::Text(&cache.tempo),
            "deck.playback.remain" => ReadValue::Text(&cache.remain),
            "deck.track.title" if !ui.track_name.trim().is_empty() => {
                ReadValue::Text(&ui.track_name)
            }
            "deck.track.source_kind" => ReadValue::Text(&cache.subtitle),
            "deck.ts.tempo" => {
                let range = f64::from(TEMPO_RANGE);
                ReadValue::Scalar((f64::from(ts.tempo) + range) / (range * 2.0))
            }
            "deck.ts.keylock" => ReadValue::Bool(keylock(deck)?),
            "deck.eq.low" => eq_value(ui.eq_bands.first())?,
            "deck.eq.mid" => eq_value(ui.eq_bands.get(1))?,
            "deck.eq.high" => eq_value(ui.eq_bands.get(2))?,
            "deck.view.zoom" => ReadValue::Scalar(cache.zoom?),
            "mixer.trim" => {
                ReadValue::Scalar(f64::from(self.state.session.mix().strips.get(index)?.trim))
            }
            "mixer.muted" => ReadValue::Bool(self.state.session.mix().strips.get(index)?.muted),
            _ => return None,
        };
        Some(value)
    }

    fn global_value(&self, endpoint: &str) -> Option<ReadValue<'a>> {
        let mix = self.state.session.mix();
        let value = match endpoint {
            "mix.crossfader" => ReadValue::Scalar(f64::from(mix.position)),
            "mix.group_master" => ReadValue::Scalar(f64::from(mix.group_master)),
            "ui.layout.decks" => ReadValue::Scalar(self.state.studio.cache.layout.index().as_()),
            _ => return None,
        };
        Some(value)
    }
}

impl Reads for StudioReads<'_> {
    fn get(&self, endpoint: &str) -> Option<ReadValue<'_>> {
        if let Some(module) = endpoint
            .strip_prefix("ui.module.")
            .and_then(|rest| rest.strip_suffix(".collapsed"))
        {
            return Some(ReadValue::Bool(
                self.state.studio.cache.collapsed.contains(module),
            ));
        }
        if endpoint == "library.tracks" {
            return Some(ReadValue::TrackList(&self.rows));
        }
        match endpoint.split_once('@') {
            Some((base, scope)) => self.deck_value(base, deck_index(scope.strip_prefix("deck=")?)?),
            None => self.global_value(endpoint),
        }
    }
}

fn eq_value(db: Option<&f32>) -> Option<ReadValue<'static>> {
    let db = *db?;
    let normalized = (db - EQ_MIN_DB) / (EQ_MAX_DB - EQ_MIN_DB);
    Some(ReadValue::Scalar(f64::from(normalized.clamp(0.0, 1.0))))
}

fn analysis_bpm(ui: &crate::state::UiState) -> Option<f32> {
    let bpm = ui.analysis.as_ref()?.beat()?.bpm();
    bpm.is_finite().then(|| bpm.as_())
}

/// Key lock is only actionable with a native stretch backend compiled in;
/// without one the endpoint reads as absent and the chip stays inert.
fn keylock(deck: &DeckUi) -> Option<bool> {
    const KEYLOCK_UI: bool = cfg!(any(
        feature = "stretch-signalsmith",
        feature = "stretch-bungee"
    ));
    KEYLOCK_UI.then(|| deck.controller.stretch().keylock())
}
