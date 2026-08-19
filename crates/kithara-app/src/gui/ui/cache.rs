use std::collections::BTreeSet;

use kithara::audio::Waveform;
use kithara_ui::render::WaveBucket;
use num_traits::cast::{AsPrimitive, ToPrimitive};

use super::{menu::MenuState, modules::Modules, scope::deck_letter, window::WindowState};
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

/// View state the host owns, re-derived from the deck snapshots once a frame.
#[derive(Default, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ViewCache {
    pub(in crate::gui) deck_marks: CatalogRowMarks,
    pub(in crate::gui) collapsed: CollapsedModules,
    pub(in crate::gui) drag: Option<usize>,
    /// Which source group the library lists and what its browser search box
    /// narrows the tree to. The tree and the context bar both select the group;
    /// it has one owner here and no second copy.
    pub(in crate::gui) library: LibraryView,
    pub(in crate::gui) menu: MenuState,
    pub(in crate::gui) modules: Modules,
    pub(in crate::gui) stage: StageView,
    pub(in crate::gui) window: WindowState,
    #[field(get, vis = "pub(in crate::gui)", copy)]
    layout: DeckLayout,

    hover_deck: Option<usize>,
    #[field(get, vis = "pub(in crate::gui)")]
    decks: Vec<DeckCache>,

    #[field(get, vis = "pub(crate)")]
    focus_deck: usize,
}

/// The visualiser preset in force, and the BPM window the tempo map is drawn
/// against. Both are the host's alone: no engine value moves them.
#[derive(Debug, PartialEq)]
pub(in crate::gui) struct StageView {
    pub(in crate::gui) preset: u32,
    pub(in crate::gui) window: (f32, f32),
}

impl Default for StageView {
    fn default() -> Self {
        Self {
            preset: 0,
            window: (0.0, 1.0),
        }
    }
}

impl StageView {
    /// The BPM span the window is a fraction of.
    pub(in crate::gui) const BPM_FLOOR: f32 = 60.0;
    pub(in crate::gui) const BPM_CEILING: f32 = 200.0;

    /// The window's lower and upper edge in BPM.
    pub(in crate::gui) fn bpm_window(&self) -> (f32, f32) {
        let span = Self::BPM_CEILING - Self::BPM_FLOOR;
        (
            Self::BPM_FLOOR + self.window.0 * span,
            Self::BPM_FLOOR + self.window.1 * span,
        )
    }

    /// Move one edge, keeping the pair ordered so the map is never asked to
    /// draw an axis that runs backwards.
    pub(in crate::gui) fn set_edge(&mut self, edge: WindowEdge, at: f32) {
        let at = at.clamp(0.0, 1.0);
        match edge {
            WindowEdge::Min => self.window.0 = at.min(self.window.1),
            WindowEdge::Max => self.window.1 = at.max(self.window.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::gui) enum WindowEdge {
    Min,
    Max,
}

/// What the library browser is showing.
#[derive(Default)]
pub(in crate::gui) struct LibraryView {
    pub(in crate::gui) query: String,
    pub(in crate::gui) scope: LibraryScope,
}

impl LibraryView {
    /// The groups the browser is listing, in the order it lists them. The tree
    /// draws these and the host resolves a row the user picked back through the
    /// same order, so a search can never make the two disagree.
    pub(in crate::gui) fn groups(&self) -> impl Iterator<Item = LibraryScope> + '_ {
        let query = self.query.trim().to_lowercase();
        LibraryScope::ALL
            .into_iter()
            .filter(move |group| query.is_empty() || group.label().to_lowercase().contains(&query))
    }
}

/// The source groups the catalog can be listed by. `Local` is anything the
/// catalog resolved to a path rather than to a network source, which is the
/// only division the entries themselves carry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::gui) enum LibraryScope {
    #[default]
    All,
    Local,
    Stream,
}

impl LibraryScope {
    pub(in crate::gui) const ALL: [Self; 3] = [Self::All, Self::Local, Self::Stream];

    pub(in crate::gui) const fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Local => "LOCAL",
            Self::Stream => "STREAM",
        }
    }

    pub(in crate::gui) const fn index(self) -> usize {
        match self {
            Self::All => 0,
            Self::Local => 1,
            Self::Stream => 2,
        }
    }

    /// A catalog entry belongs to this group.
    pub(in crate::gui) fn holds(self, entry: &CatalogEntry) -> bool {
        let streamed = entry.source.contains("://") && !entry.source.starts_with("file://");
        match self {
            Self::All => true,
            Self::Local => !streamed,
            Self::Stream => streamed,
        }
    }
}

/// Deck bodies the app lays out; the session keeps every deck either way.
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

    /// The menu addresses a layout by the deck count it lays out.
    pub(in crate::gui) const fn from_decks(decks: usize) -> Option<Self> {
        match decks {
            1 => Some(Self::Single),
            2 => Some(Self::Dual),
            _ => None,
        }
    }

    pub(in crate::gui) const fn label(self) -> &'static str {
        match self {
            Self::Single => "1 DECK",
            Self::Dual => "2 DECKS",
        }
    }
}

#[derive(Default)]
pub(in crate::gui) struct DeckCache {
    pub(in crate::gui) view: DeckViewState,
    pub(in crate::gui) bpm: String,
    pub(in crate::gui) quality: String,
    pub(in crate::gui) remain: String,
    pub(in crate::gui) subtitle: String,
    pub(in crate::gui) tempo: String,
    pub(in crate::gui) wave: Vec<WaveBucket>,
    /// What the toolkit calls the run of buckets currently in `wave`.
    ///
    /// Moved by whoever writes `wave`, so a viewer holding a copy can tell
    /// the two apart without walking six figures of buckets every frame.
    pub(in crate::gui) wave_revision: u64,
    wave_src: Option<usize>,
}

#[derive(Default)]
pub(in crate::gui) struct DeckViewState {
    pub(in crate::gui) zoom: Option<f64>,
    pub(in crate::gui) quality_menu: bool,
    pub(in crate::gui) eq_menu_open: bool,
}

#[derive(Default)]
pub(in crate::gui) struct CatalogRowMarks(Vec<String>);

#[derive(Default)]
pub(in crate::gui) struct CollapsedModules(BTreeSet<String>);

impl ViewCache {
    pub(in crate::gui) fn deck_mut(&mut self, index: usize) -> Option<&mut DeckCache> {
        self.decks.get_mut(index)
    }

    pub(in crate::gui) const fn drag_target(&self) -> Option<usize> {
        if self.drag.is_some() {
            self.hover_deck
        } else {
            None
        }
    }

    pub(in crate::gui) fn set_eq_menu_open(&mut self, index: usize, open: bool) -> Option<()> {
        self.deck_mut(index)?.view.eq_menu_open = open;
        Some(())
    }

    pub(in crate::gui) fn close_eq_menus(&mut self) {
        for deck in &mut self.decks {
            deck.view.eq_menu_open = false;
        }
    }

    pub(crate) const fn laid_out_decks(&self) -> usize {
        self.layout.decks()
    }

    pub(crate) fn refresh(&mut self, decks: &Decks, catalog: &Catalog) {
        self.decks.resize_with(decks.len(), Default::default);
        for (cache, deck) in self.decks.iter_mut().zip(decks.iter()) {
            cache.refresh(deck);
        }
        self.deck_marks.refresh(decks, catalog);
        self.window.refresh(self.layout, &self.modules);
    }

    pub(in crate::gui) fn set_hover_deck(&mut self, deck: usize, over: bool) {
        if over {
            self.hover_deck = Some(deck);
        } else if self.hover_deck == Some(deck) {
            self.hover_deck = None;
        }
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

    pub(in crate::gui) fn take_drop(&mut self) -> Option<(usize, usize)> {
        let row = self.drag.take()?;
        let deck = self.hover_deck?;
        self.focus_deck = deck;
        Some((row, deck))
    }

    pub(in crate::gui) fn toggle_module(&mut self, module: String) {
        self.collapsed.toggle(module);
    }

    #[cfg(test)]
    pub(in crate::gui) fn with_decks(decks: usize) -> Self {
        let mut cache = Self::default();
        cache.decks.resize_with(decks, DeckCache::default);
        cache
    }
}

impl DeckCache {
    fn refresh(&mut self, deck: &DeckUi) {
        let ts = deck.view.timestretch;
        self.tempo = format!("{:+.1}%", ts.tempo);
        self.bpm = format_bpm(analysis_bpm(&deck.ui), ts.speed());
        self.remain = format_remain(&deck.ui);
        self.subtitle = track_subtitle(&deck.ui);
        self.quality = format_quality(&deck.ui);
        self.refresh_wave(deck.ui.analysis.as_ref());
    }

    fn refresh_wave(&mut self, analysis: Option<&TrackAnalysis>) {
        let wave = analysis.and_then(TrackAnalysis::waveform);
        let src = wave.map(|wave| wave.buckets().as_ptr().addr());
        if src == self.wave_src {
            return;
        }
        self.wave_src = src;
        self.wave_revision = self.wave_revision.wrapping_add(1);
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

fn format_quality(ui: &UiState) -> String {
    let rung = ui
        .current_variant
        .or(ui.selected_variant)
        .and_then(|index| {
            ui.abr_variants
                .iter()
                .find(|variant| variant.index == index)
        });
    match (ui.abr_mode_is_auto, rung) {
        (true, Some(rung)) => format!("AUTO\u{b7}{}", rung.label),
        (true, None) => "AUTO".to_owned(),
        (false, Some(rung)) => rung.label.clone(),
        (false, None) => String::new(),
    }
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
    use crate::{state::AbrVariant, waveform::TrackAnalysis};

    /// Version 1 plus one bucket of three band heights (0.5 = 0x3F000000).
    fn wave_of(height: u8) -> TrackAnalysis {
        let blob = [
            1, 0, 0, 0, 0, 0, 0, height, 0, 0, 0, height, 0, 0, 0, height,
        ];
        let wave = Waveform::try_from(blob.as_slice()).expect("hand-built blob is valid");
        TrackAnalysis::new(None, Some(wave), 0)
    }

    #[kithara::test]
    fn the_deck_names_the_run_of_buckets_it_just_wrote() {
        let mut cache = DeckCache::default();
        let quiet = wave_of(62);

        cache.refresh_wave(Some(&quiet));
        let named = cache.wave_revision;

        cache.refresh_wave(Some(&quiet));
        assert_eq!(cache.wave_revision, named, "nothing was written");

        cache.refresh_wave(Some(&wave_of(63)));
        assert_ne!(
            cache.wave_revision, named,
            "a new run of buckets was written"
        );
    }

    #[kithara::test]
    fn dropping_the_track_names_the_empty_run() {
        let mut cache = DeckCache::default();
        cache.refresh_wave(Some(&wave_of(63)));
        let named = cache.wave_revision;

        cache.refresh_wave(None);

        assert!(cache.wave.is_empty());
        assert_ne!(cache.wave_revision, named, "the empty run is a run too");
    }

    fn ladder() -> Vec<AbrVariant> {
        vec![
            AbrVariant {
                index: 0,
                label: "128k".to_owned(),
                detail: "128 kbps \u{b7} AAC".to_owned(),
            },
            AbrVariant {
                index: 1,
                label: "320k".to_owned(),
                detail: "320 kbps \u{b7} AAC".to_owned(),
            },
        ]
    }

    #[kithara::test]
    fn the_cell_marks_the_rung_the_ladder_chose_and_names_the_one_the_user_pinned() {
        let mut ui = UiState::empty();
        ui.abr_variants = ladder();
        ui.current_variant = Some(1);

        assert_eq!(format_quality(&ui), "AUTO\u{b7}320k");

        ui.abr_mode_is_auto = false;
        ui.selected_variant = Some(0);
        ui.current_variant = Some(0);
        assert_eq!(format_quality(&ui), "128k");
    }

    #[kithara::test]
    fn a_stream_with_no_rung_yet_still_reports_its_mode() {
        let ui = UiState::empty();

        assert_eq!(format_quality(&ui), "AUTO");
    }

    #[kithara::test]
    fn a_drop_ends_the_drag_and_keeps_the_hover() {
        let mut cache = ViewCache::default();
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
        let mut cache = ViewCache::default();
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
        let mut cache = ViewCache::default();
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
    fn a_layout_answers_to_the_deck_count_the_menu_names() {
        for layout in [DeckLayout::Single, DeckLayout::Dual] {
            assert_eq!(DeckLayout::from_decks(layout.decks()), Some(layout));
        }
        assert_eq!(DeckLayout::from_decks(3), None);
    }
}
