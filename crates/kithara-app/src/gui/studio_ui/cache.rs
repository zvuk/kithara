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
