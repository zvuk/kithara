use kithara::{
    effects::GainDb,
    ui::render::{Node, ReadValue, Scope, WaveformView},
};
use num_traits::cast::AsPrimitive;

use super::value::{Value, impl_child_node};
use crate::{
    deck::{EqMode, TempoPercent},
    engine::DeckSnapshot,
    gui::{
        deck::TEMPO_RANGE,
        ui::{cache::DeckCache, scope::deck_index},
    },
    state::AbrVariant,
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
    cache: &'a DeckCache,
    shown: &'a DeckSnapshot,
    eq_mode: EqMode,
    focused: bool,
}

impl<'a> DeckNode<'a> {
    pub(super) const fn new(
        shown: &'a DeckSnapshot,
        cache: &'a DeckCache,
        eq_mode: EqMode,
        focused: bool,
    ) -> Self {
        Self {
            cache,
            shown,
            eq_mode,
            focused,
        }
    }
}

impl_child_node!(DeckNode<'a>, |this, segment, _scope| {
    let node: Box<dyn Node<'a> + 'a> = match segment {
        "playback" => Box::new(PlaybackNode {
            shown: this.shown,
            cache: this.cache,
        }),
        "track" => Box::new(TrackNode {
            shown: this.shown,
            cache: this.cache,
        }),
        "tempo" => Box::new(TempoNode {
            tempo: this.shown.tempo,
        }),
        "eq" => Box::new(EqNode {
            shown: this.shown,
            cache: this.cache,
            mode: this.eq_mode,
        }),
        "stream" => Box::new(StreamNode {
            shown: this.shown,
            cache: this.cache,
        }),
        "view" => Box::new(ViewNode { cache: this.cache }),
        "focused" => Box::new(Value(ReadValue::Bool(this.focused))),
        _ => return None,
    };
    Some(node)
});

#[derive(Clone, Copy)]
struct PlaybackNode<'a> {
    cache: &'a DeckCache,
    shown: &'a DeckSnapshot,
}

impl PlaybackNode<'_> {
    fn normalized(self) -> f64 {
        let duration = self.shown.duration.max(0.0);
        if duration > 0.0 {
            (self.shown.position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

impl_child_node!(PlaybackNode<'a>, |this, segment, _scope| {
    let value = match segment {
        "waveform" => ReadValue::Waveform(WaveformView {
            buckets: &this.cache.wave,
            revision: this.cache.wave_revision,
            beats: &this.shown.analysis.beats,
            downbeats: &this.shown.analysis.downbeats,
            unready: &this.shown.analysis.unready,
            bpm: this.shown.analysis.bpm,
            r#loop: None,
            cues: &[],
        }),
        "playing" => ReadValue::Bool(this.shown.playing),
        "position_secs" => ReadValue::Scalar(this.shown.position.max(0.0)),
        "duration_secs" => ReadValue::Scalar(this.shown.duration.max(0.0)),
        "position_normalized" => ReadValue::Scalar(this.normalized()),
        "tempo" => ReadValue::Text(&this.cache.tempo),
        "bpm" => ReadValue::Text(&this.cache.bpm),
        "remain" => ReadValue::Text(&this.cache.remain),
        _ => return None,
    };
    Some(Box::new(Value(value)))
});

#[derive(Clone, Copy)]
struct TrackNode<'a> {
    cache: &'a DeckCache,
    shown: &'a DeckSnapshot,
}

impl<'a> Node<'a> for TrackNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "title" => ReadValue::Text(title(self.shown)?),
            "source_kind" => ReadValue::Text(&self.cache.subtitle),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct TempoNode {
    tempo: TempoPercent,
}

impl<'a> Node<'a> for TempoNode {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let range = f64::from(TEMPO_RANGE);
        let value = match segment {
            "rate" => ReadValue::Scalar((f64::from(f32::from(self.tempo)) + range) / (range * 2.0)),
            _ => return None,
        };
        Some(Box::new(Value(value)))
    }
}

#[derive(Clone, Copy)]
struct StreamNode<'a> {
    cache: &'a DeckCache,
    shown: &'a DeckSnapshot,
}

impl<'a> StreamNode<'a> {
    const AUTO_SLOT: &'static str = "auto";

    fn active(self, slot: &str) -> bool {
        if slot == Self::AUTO_SLOT {
            return self.shown.stream.is_auto;
        }
        let picked = self.shown.stream.selected;
        !self.shown.stream.is_auto
            && self
                .rung(slot)
                .is_some_and(|rung| picked == Some(rung.index))
    }

    fn rung(self, slot: &str) -> Option<&'a AbrVariant> {
        self.shown.stream.variants.get(slot.parse::<usize>().ok()?)
    }
}

impl_child_node!(StreamNode<'a>, |this, segment, scope| {
    let stream = *this;
    let value = match segment {
        "quality" => ReadValue::Text(&stream.cache.quality),
        "quality_menu" => ReadValue::Bool(stream.cache.view.quality_menu),
        "quality_hidden" => ReadValue::Bool(stream.shown.stream.variants.is_empty()),
        "variant_active" => ReadValue::Bool(stream.active(scope.get("variant")?)),
        "variant_hidden" => ReadValue::Bool(stream.rung(scope.get("variant")?).is_none()),
        "variant_label" => ReadValue::Text(&stream.rung(scope.get("variant")?)?.label),
        "variant_sub" => ReadValue::Text(&stream.rung(scope.get("variant")?)?.detail),
        _ => return None,
    };
    Some(Box::new(Value(value)))
});

#[derive(Clone, Copy)]
struct EqNode<'a> {
    cache: &'a DeckCache,
    shown: &'a DeckSnapshot,
    mode: EqMode,
}

impl<'a> Node<'a> for EqNode<'a> {
    fn child(&self, segment: &str, scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "menu_open" => ReadValue::Bool(self.cache.view.eq_menu_open),
            "bands" => ReadValue::Scalar(self.mode.bands().len().as_()),
            "selected" => ReadValue::Bool(self.drawn(scope.get("bands")?)),
            band => eq_value(self.shown.eq_bands.get(self.mode.band(band)?))?,
        };
        Some(Box::new(Value(value)))
    }
}

impl EqNode<'_> {
    fn drawn(&self, bands: &str) -> bool {
        bands.parse::<usize>() == Ok(self.mode.bands().len())
    }
}

#[derive(Clone, Copy)]
struct ViewNode<'a> {
    cache: &'a DeckCache,
}

impl<'a> Node<'a> for ViewNode<'a> {
    fn child(&self, segment: &str, _scope: Scope<'_>) -> Option<Box<dyn Node<'a> + 'a>> {
        let value = match segment {
            "zoom" => ReadValue::Scalar(self.cache.view.zoom?),
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
            .map(|deck| deck.shown.engine_load.load())
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

fn title(ui: &DeckSnapshot) -> Option<&str> {
    (!ui.track_name.trim().is_empty()).then_some(&ui.track_name)
}

fn eq_value(db: Option<&GainDb>) -> Option<ReadValue<'static>> {
    Some(ReadValue::Scalar(f64::from(db?.knob())))
}
