use kithara_ui::render::{Node, ReadValue, Scope, WaveformView};
use num_traits::cast::AsPrimitive;

use super::value::Value;
use crate::{
    deck::EQ_BANDS,
    gui::{
        deck::{DeckView, TEMPO_RANGE, TimestretchState},
        studio_ui::{
            cache::{DeckCache, analysis_bpm},
            endpoints::{EQ_MAX_DB, EQ_MIN_DB},
            scope::deck_index,
        },
        view::playhead,
    },
    state::UiState,
};

#[derive(Clone, Copy)]
pub(super) struct DecksNode<'a> {
    decks: &'a [DeckNode<'a>],
}

impl<'a> DecksNode<'a> {
    pub(super) const fn new(decks: &'a [DeckNode<'a>]) -> Self {
        Self { decks }
    }

    fn deck(self, scope: Scope<'_>) -> Option<DeckNode<'a>> {
        self.decks.get(deck_index(scope.get("deck")?)?).copied()
    }
}

impl<'a> Node<'a> for DecksNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        self.deck(scope)?.child(segment, scope)
    }
}

#[derive(Clone, Copy)]
pub(super) struct DeckNode<'a> {
    ui: &'a UiState,
    view: DeckView,
    cache: &'a DeckCache,
    focused: bool,
}

impl<'a> DeckNode<'a> {
    pub(super) const fn new(
        ui: &'a UiState,
        view: DeckView,
        cache: &'a DeckCache,
        focused: bool,
    ) -> Self {
        Self {
            ui,
            view,
            cache,
            focused,
        }
    }
}

impl<'a> Node<'a> for DeckNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let node: Box<dyn Node<'a> + 'a> = match segment {
            "playback" => Box::new(PlaybackNode {
                ui: self.ui,
                cache: self.cache,
            }),
            "track" => Box::new(TrackNode {
                ui: self.ui,
                cache: self.cache,
            }),
            "tempo" => Box::new(TempoNode {
                timestretch: self.view.timestretch,
            }),
            "eq" => Box::new(EqNode { ui: self.ui }),
            "view" => Box::new(ViewNode { cache: self.cache }),
            "focused" => Box::new(Value(ReadValue::Bool(self.focused))),
            _ => return None,
        };
        Some(node)
    }
}

#[derive(Clone, Copy)]
struct PlaybackNode<'a> {
    ui: &'a UiState,
    cache: &'a DeckCache,
}

impl PlaybackNode<'_> {
    fn normalized(self) -> f64 {
        let duration = self.ui.duration.max(0.0);
        if duration > 0.0 {
            (playhead(self.ui) / duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

impl<'a> Node<'a> for PlaybackNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "waveform" => ReadValue::Waveform(WaveformView {
                buckets: &self.cache.wave,
                beats: &self.ui.beat_marks,
                downbeats: &self.ui.downbeat_marks,
                bpm: analysis_bpm(self.ui),
                r#loop: None,
                cues: &[],
            }),
            "playing" => ReadValue::Bool(self.ui.playing),
            "position_secs" => ReadValue::Scalar(playhead(self.ui).max(0.0)),
            "duration_secs" => ReadValue::Scalar(self.ui.duration.max(0.0)),
            "position_normalized" => ReadValue::Scalar(self.normalized()),
            "tempo" => ReadValue::Text(&self.cache.tempo),
            "bpm" => ReadValue::Text(&self.cache.bpm),
            "remain" => ReadValue::Text(&self.cache.remain),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct TrackNode<'a> {
    ui: &'a UiState,
    cache: &'a DeckCache,
}

impl<'a> Node<'a> for TrackNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "title" => ReadValue::Text(title(self.ui)?),
            "source_kind" => ReadValue::Text(&self.cache.subtitle),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct TempoNode {
    timestretch: TimestretchState,
}

impl<'a> Node<'a> for TempoNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let range = f64::from(TEMPO_RANGE);
        let value = match segment {
            "rate" => {
                ReadValue::Scalar((f64::from(self.timestretch.tempo) + range) / (range * 2.0))
            }
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct EqNode<'a> {
    ui: &'a UiState,
}

impl<'a> Node<'a> for EqNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let band = EQ_BANDS.iter().position(|knob| *knob == segment)?;
        let value = eq_value(self.ui.eq_bands.get(band))?;
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct ViewNode<'a> {
    cache: &'a DeckCache,
}

impl<'a> Node<'a> for ViewNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "zoom" => ReadValue::Scalar(self.cache.zoom?),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
pub(super) struct EngineNode {
    load: f32,
}

impl EngineNode {
    pub(super) fn new(decks: &[DeckNode<'_>]) -> Self {
        let load = decks
            .iter()
            .map(|deck| deck.ui.engine_load.load())
            .fold(0.0, f32::max);
        Self { load }
    }
}

impl<'a> Node<'a> for EngineNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "load" => ReadValue::Scalar(self.load.as_()),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

fn title(ui: &UiState) -> Option<&str> {
    (!ui.track_name.trim().is_empty()).then_some(ui.track_name.as_str())
}

fn eq_value(db: Option<&f32>) -> Option<ReadValue<'static>> {
    let db = *db?;
    let normalized = (db - EQ_MIN_DB) / (EQ_MAX_DB - EQ_MIN_DB);
    Some(ReadValue::Scalar(f64::from(normalized.clamp(0.0, 1.0))))
}
