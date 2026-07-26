use std::collections::BTreeSet;

use kithara::audio::Waveform;
use kithara_ui::render::WaveBucket;
use num_traits::cast::ToPrimitive;

use crate::{
    catalog::{Catalog, is_loaded},
    gui::{app::Decks, deck::DeckUi, view::track_subtitle},
    state::UiState,
    waveform::TrackAnalysis,
};

/// Studio state owned by the host and refreshed once per frame: converted
/// waveform columns, formatted strings the renderer borrows, and the view
/// state the compiled UI reads back (zoom, collapsed modules, deck layout).
#[derive(Default)]
pub(crate) struct StudioCache {
    pub(super) decks: Vec<DeckCache>,
    /// Per catalog row: channel letters of the decks the row is loaded on.
    pub(super) deck_marks: Vec<String>,
    pub(super) collapsed: BTreeSet<String>,
    pub(crate) layout: DeckLayout,
    /// Catalog row the pointer is carrying out of the library.
    pub(super) drag: Option<usize>,
    /// Deck the pointer is over, tracked whether or not a drag is in flight.
    pub(super) hover_deck: Option<usize>,
}

/// How many decks the studio lays out. The top bar switches it; the session
/// keeps every deck either way.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DeckLayout {
    Single,
    #[default]
    Dual,
}

impl DeckLayout {
    /// Deck bodies the layout lays out; the session may hold more.
    pub(crate) const fn decks(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Dual => 2,
        }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Single => 0,
            Self::Dual => 1,
        }
    }

    pub(crate) const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Single),
            1 => Some(Self::Dual),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct DeckCache {
    pub(super) wave: Vec<WaveBucket>,
    /// Revision stamp of the converted waveform: the source slice address.
    wave_src: Option<usize>,
    pub(super) tempo: String,
    /// Time left in the track, `−MM:SS`, as the overview row shows it.
    pub(super) remain: String,
    pub(super) subtitle: String,
    pub(super) zoom: Option<f64>,
}

impl StudioCache {
    pub(crate) fn toggle_module(&mut self, module: String) {
        if !self.collapsed.remove(&module) {
            self.collapsed.insert(module);
        }
    }

    pub(super) fn deck(&self, index: usize) -> Option<&DeckCache> {
        self.decks.get(index)
    }

    /// The pointer entered or left a deck. Hover is tracked on its own: a drag
    /// that starts with the pointer already inside a deck gets no fresh enter.
    pub(super) fn set_hover_deck(&mut self, deck: usize, over: bool) {
        if over {
            self.hover_deck = Some(deck);
        } else if self.hover_deck == Some(deck) {
            self.hover_deck = None;
        }
    }

    /// A dragged row is over this deck.
    pub(super) fn drag_over(&self, deck: usize) -> bool {
        self.drag.is_some() && self.hover_deck == Some(deck)
    }

    /// End the drag and report where it landed: the row it carried and the
    /// deck under the pointer, if it was released over one.
    pub(super) fn take_drop(&mut self) -> Option<(usize, usize)> {
        let track = self.drag.take()?;
        Some((track, self.hover_deck?))
    }

    pub(super) fn deck_mut(&mut self, index: usize) -> Option<&mut DeckCache> {
        self.decks.get_mut(index)
    }

    /// Refresh the borrowed-by-render state from the current snapshots.
    /// Called once per frame after the deck snapshots are taken.
    pub(crate) fn refresh(&mut self, decks: &Decks, catalog: &Catalog) {
        self.decks
            .resize_with(decks.iter().count(), Default::default);
        for (cache, deck) in self.decks.iter_mut().zip(decks.iter()) {
            cache.refresh(deck);
        }
        self.refresh_deck_marks(decks, catalog);
    }

    fn refresh_deck_marks(&mut self, decks: &Decks, catalog: &Catalog) {
        const CHANNELS: [char; 4] = ['A', 'B', 'C', 'D'];
        self.deck_marks.clear();
        for entry in catalog.entries() {
            let mut marks = String::new();
            for (at, deck) in decks.iter().enumerate() {
                if is_loaded(deck.controller.queue(), entry) {
                    marks.push(*CHANNELS.get(at).unwrap_or(&'\u{00b7}'));
                }
            }
            self.deck_marks.push(marks);
        }
    }
}

impl DeckCache {
    fn refresh(&mut self, deck: &DeckUi) {
        let ts = deck.view.timestretch;
        self.tempo = format!("{:+.1}%", ts.tempo);
        self.remain = format_remain(&deck.ui);
        self.subtitle = track_subtitle(&deck.ui);
        self.refresh_wave(deck.ui.analysis.as_ref());
    }

    /// Convert the analysed waveform into renderer columns only when the
    /// underlying `Arc` changes; the conversion is native-resolution sized.
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

/// The playhead the UI shows: while the user drags, that is the seek target.
pub(super) fn head(ui: &UiState) -> f64 {
    if ui.is_seeking {
        ui.seek_position
    } else {
        ui.position
    }
}

fn format_remain(ui: &UiState) -> String {
    let left = (ui.duration - head(ui)).max(0.0);
    let total = left.floor().to_u64().unwrap_or(0);
    format!("\u{2212}{:02}:{:02}", total / 60, total % 60)
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

    /// Hover outlives one drag: dropping on a deck the pointer stays inside
    /// must not stop the next drag from landing there.
    #[kithara::test]
    fn a_drop_ends_the_drag_and_keeps_the_hover() {
        let mut cache = StudioCache::default();
        cache.set_hover_deck(1, true);
        cache.drag = Some(4);

        assert!(cache.drag_over(1));
        assert!(!cache.drag_over(0));
        assert_eq!(cache.take_drop(), Some((4, 1)));
        assert!(!cache.drag_over(1), "the drag is over");

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
}
