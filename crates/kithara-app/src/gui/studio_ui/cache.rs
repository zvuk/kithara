use std::collections::BTreeSet;

use kithara::audio::Waveform;
use kithara_ui::render::WaveBucket;
use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::scope::deck_letter;
use crate::{
    catalog::{Catalog, CatalogEntry, is_loaded},
    gui::{
        app::Decks,
        deck::DeckUi,
        view::{playhead, track_subtitle},
    },
    state::UiState,
    waveform::TrackAnalysis,
};

/// Studio state the host owns, re-derived from the deck snapshots once a frame.
#[derive(Default)]
pub(crate) struct StudioCache {
    decks: Vec<DeckCache>,
    layout: DeckLayout,
    hover_deck: Option<usize>,
    focus_deck: usize,

    pub(in crate::gui) deck_marks: CatalogRowMarks,
    pub(in crate::gui) drag: Option<usize>,

    pub(in crate::gui) collapsed: CollapsedModules,
}

/// Deck bodies the studio lays out; the session keeps every deck either way.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::gui) enum DeckLayout {
    Single,
    #[default]
    Dual,
}

impl DeckLayout {
    pub(in crate::gui) const fn decks(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Dual => 2,
        }
    }

    pub(in crate::gui) const fn index(self) -> usize {
        match self {
            Self::Single => 0,
            Self::Dual => 1,
        }
    }

    pub(in crate::gui) const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Single),
            1 => Some(Self::Dual),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(in crate::gui) struct DeckCache {
    pub(in crate::gui) wave: Vec<WaveBucket>,
    wave_src: Option<usize>,
    pub(in crate::gui) tempo: String,
    pub(in crate::gui) bpm: String,
    pub(in crate::gui) remain: String,
    pub(in crate::gui) subtitle: String,
    pub(in crate::gui) zoom: Option<f64>,
}

#[derive(Default)]
pub(in crate::gui) struct CatalogRowMarks(Vec<String>);

#[derive(Default)]
pub(in crate::gui) struct CollapsedModules(BTreeSet<String>);

impl StudioCache {
    #[cfg(test)]
    pub(in crate::gui) fn with_decks(decks: usize) -> Self {
        let mut cache = Self::default();
        cache.decks.resize_with(decks, DeckCache::default);
        cache
    }

    pub(in crate::gui) fn toggle_module(&mut self, module: String) {
        self.collapsed.toggle(module);
    }

    pub(in crate::gui) fn decks(&self) -> &[DeckCache] {
        &self.decks
    }

    pub(in crate::gui) fn deck_mut(&mut self, index: usize) -> Option<&mut DeckCache> {
        self.decks.get_mut(index)
    }

    pub(in crate::gui) const fn layout(&self) -> DeckLayout {
        self.layout
    }

    pub(crate) const fn laid_out_decks(&self) -> usize {
        self.layout.decks()
    }

    pub(crate) const fn focus_deck(&self) -> usize {
        self.focus_deck
    }

    pub(in crate::gui) fn set_layout(&mut self, layout: DeckLayout) {
        self.layout = layout;
        if self.hover_deck.is_some_and(|deck| deck >= layout.decks()) {
            self.hover_deck = None;
        }
        if self.focus_deck >= layout.decks() {
            self.focus_deck = 0;
        }
    }

    pub(in crate::gui) fn set_hover_deck(&mut self, deck: usize, over: bool) {
        if over {
            self.hover_deck = Some(deck);
        } else if self.hover_deck == Some(deck) {
            self.hover_deck = None;
        }
    }

    pub(in crate::gui) const fn drag_target(&self) -> Option<usize> {
        if self.drag.is_some() {
            self.hover_deck
        } else {
            None
        }
    }

    pub(in crate::gui) fn take_drop(&mut self) -> Option<(usize, usize)> {
        let row = self.drag.take()?;
        let deck = self.hover_deck?;
        self.focus_deck = deck;
        Some((row, deck))
    }

    pub(crate) fn refresh(&mut self, decks: &Decks, catalog: &Catalog) {
        self.decks
            .resize_with(decks.iter().count(), Default::default);
        for (cache, deck) in self.decks.iter_mut().zip(decks.iter()) {
            cache.refresh(deck);
        }
        self.deck_marks.refresh(decks, catalog);
    }
}

impl DeckCache {
    fn refresh(&mut self, deck: &DeckUi) {
        let ts = deck.view.timestretch;
        self.tempo = format!("{:+.1}%", ts.tempo);
        self.bpm = format_bpm(analysis_bpm(&deck.ui), ts.speed());
        self.remain = format_remain(&deck.ui);
        self.subtitle = track_subtitle(&deck.ui);
        self.refresh_wave(deck.ui.analysis.as_ref());
    }

    fn refresh_wave(&mut self, analysis: Option<&TrackAnalysis>) {
        let wave = analysis.and_then(TrackAnalysis::waveform);
        let src = wave.map(|wave| wave.buckets().as_ptr().addr());
        if src == self.wave_src {
            return;
        }
        self.wave_src = src;
        self.wave.clear();
        if let Some(wave) = wave {
            self.wave.extend(waveform_buckets(wave));
        }
    }
}

impl CatalogRowMarks {
    pub(in crate::gui) fn get(&self, row: usize) -> Option<&String> {
        self.0.get(row)
    }

    fn refresh(&mut self, decks: &Decks, catalog: &Catalog) {
        self.0.clear();
        self.0.extend(
            catalog
                .entries()
                .iter()
                .map(|entry| loaded_deck_letters(entry, decks)),
        );
    }
}

impl CollapsedModules {
    pub(in crate::gui) fn contains(&self, module: &str) -> bool {
        self.0.contains(module)
    }

    fn toggle(&mut self, module: String) {
        if !self.0.remove(&module) {
            self.0.insert(module);
        }
    }
}

fn loaded_deck_letters(entry: &CatalogEntry, decks: &Decks) -> String {
    decks
        .iter()
        .enumerate()
        .filter(|(_, deck)| is_loaded(deck.controller.queue(), entry))
        .filter_map(|(at, _)| deck_letter(at))
        .map(|letter| letter.to_ascii_uppercase())
        .collect()
}

pub(in crate::gui) fn analysis_bpm(ui: &UiState) -> Option<f32> {
    let bpm = ui.analysis.as_ref()?.beat()?.bpm();
    bpm.is_finite().then(|| bpm.as_())
}

fn format_bpm(source: Option<f32>, speed: f32) -> String {
    const EM_DASH: char = '\u{2014}';

    source.map_or_else(|| EM_DASH.to_string(), |bpm| format!("{:.1}", bpm * speed))
}

fn format_remain(ui: &UiState) -> String {
    const MINUS_SIGN: char = '\u{2212}';

    let left = (ui.duration - playhead(ui)).max(0.0);
    let total = left.floor().to_u64().unwrap_or(0);
    format!("{MINUS_SIGN}{:02}:{:02}", total / 60, total % 60)
}

fn waveform_buckets(wave: &Waveform) -> impl Iterator<Item = WaveBucket> + '_ {
    wave.buckets().iter().map(|bucket| WaveBucket {
        low: bucket.low(),
        mid: bucket.mid(),
        high: bucket.high(),
    })
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn a_drop_ends_the_drag_and_keeps_the_hover() {
        let mut cache = StudioCache::default();
        cache.set_hover_deck(1, true);
        cache.drag = Some(4);

        assert_eq!(cache.drag_target(), Some(1));
        assert_eq!(cache.take_drop(), Some((4, 1)));
        assert_eq!(cache.drag_target(), None, "the drag is over");

        cache.drag = Some(7);
        assert_eq!(cache.take_drop(), Some((7, 1)));
    }

    #[kithara::test]
    fn a_drop_outside_every_deck_lands_nowhere() {
        let mut cache = StudioCache::default();
        cache.set_hover_deck(0, true);
        cache.set_hover_deck(1, false);
        cache.drag = Some(2);

        assert_eq!(cache.hover_deck, Some(0), "another deck's exit is not ours");
        cache.set_hover_deck(0, false);
        assert_eq!(cache.take_drop(), None);
        assert_eq!(cache.drag, None, "a drop always ends the drag");
    }

    #[kithara::test]
    fn a_drop_focuses_the_deck_it_landed_on() {
        let mut cache = StudioCache::default();
        assert_eq!(cache.focus_deck(), 0);

        cache.set_hover_deck(1, true);
        cache.drag = Some(2);
        assert_eq!(cache.take_drop(), Some((2, 1)));
        assert_eq!(cache.focus_deck(), 1);

        cache.set_hover_deck(1, false);
        cache.drag = Some(5);
        assert_eq!(cache.take_drop(), None);
        assert_eq!(cache.focus_deck(), 1, "a drop on nothing focuses nothing");
    }

    #[kithara::test]
    fn layout_indices_round_trip() {
        for layout in [DeckLayout::Single, DeckLayout::Dual] {
            assert_eq!(DeckLayout::from_index(layout.index()), Some(layout));
        }
        assert_eq!(DeckLayout::from_index(2), None);
    }
}
