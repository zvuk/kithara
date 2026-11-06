use std::collections::BTreeMap;

use kithara::{analysis::Waveform, platform::tokio::runtime::Handle, ui::render::WaveBucket};
use num_traits::cast::ToPrimitive;

use super::{artwork::Artwork, modules::Modules, window::WindowState};
use crate::{
    analysis::{ArtifactId, TrackArtifacts},
    deck::{DeckSet, TempoPercent},
    engine::{DeckSnapshot, EngineSnapshot},
    gui::view::track_subtitle,
};

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ViewCache {
    pub(in crate::gui) modules: Modules,
    pub(in crate::gui) stage: StageView,
    pub(in crate::gui) window: WindowState,
    pub(in crate::gui) master_bpm: String,
    #[field(get, vis = "pub(in crate::gui)", copy)]
    layout: DeckLayout,

    decks: BTreeMap<&'static str, DeckCache>,

    #[field(get, vis = "pub(crate)", copy)]
    focus_deck: &'static str,
}

impl Default for ViewCache {
    fn default() -> Self {
        let layout = DeckLayout::default();

        Self {
            modules: Modules::default(),
            stage: StageView::default(),
            window: WindowState::default(),
            master_bpm: String::new(),
            decks: BTreeMap::new(),
            focus_deck: layout.letters()[0],
            layout,
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub(in crate::gui) struct StageView {
    pub(in crate::gui) preset: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::gui) enum DeckLayout {
    Single,
    #[default]
    Dual,
}

impl DeckLayout {
    /// The letters of the decks a layout shows, in the order it shows them.
    pub(in crate::gui) const fn letters(self) -> &'static [&'static str] {
        match self {
            Self::Single => DeckSet::LETTERS.split_at(1).0,
            Self::Dual => &DeckSet::LETTERS,
        }
    }

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
    pub(in crate::gui) artwork: Artwork,
    pub(in crate::gui) view: DeckViewState,
    pub(in crate::gui) bpm: String,
    pub(in crate::gui) quality: String,
    pub(in crate::gui) remain: String,
    pub(in crate::gui) subtitle: String,
    pub(in crate::gui) tempo: String,
    pub(in crate::gui) wave: Vec<WaveBucket>,
    pub(in crate::gui) wave_revision: u64,
    wave_src: Option<ArtifactId>,
}

#[derive(Default)]
pub(in crate::gui) struct DeckViewState {
    pub(in crate::gui) zoom: Option<f64>,
}

impl ViewCache {
    delegate::delegate! {
        to self.decks {
            #[call(get)]
            pub(in crate::gui) fn deck(&self, letter: &str) -> Option<&DeckCache>;
            #[call(get_mut)]
            pub(in crate::gui) fn deck_mut(&mut self, letter: &str) -> Option<&mut DeckCache>;
        }
    }

    pub(crate) fn refresh(&mut self, snapshot: &EngineSnapshot, runtime: &Handle) {
        self.decks
            .retain(|letter, _| snapshot.lettered(letter).is_some());

        for deck in &snapshot.decks {
            self.decks
                .entry(deck.letter)
                .or_default()
                .refresh(deck, runtime);
        }

        self.master_bpm = format!("{:.2}", f64::from(snapshot.tempo));
        self.window.refresh(self.layout, &self.modules);
    }

    pub(in crate::gui) fn set_layout(&mut self, layout: DeckLayout) {
        self.layout = layout;
        if !layout.letters().contains(&self.focus_deck) {
            self.focus_deck = layout.letters()[0];
        }
    }

    pub(in crate::gui) fn focus(&mut self, letter: &str) {
        if let Some(laid_out) = self.layout.letters().iter().find(|laid| **laid == letter) {
            self.focus_deck = laid_out;
        }
    }

    #[cfg(test)]
    pub(in crate::gui) fn with_decks(letters: &[&'static str]) -> Self {
        let mut cache = Self::default();

        for letter in letters {
            cache.decks.insert(letter, DeckCache::default());
        }

        cache
    }
}

impl DeckCache {
    fn refresh(&mut self, deck: &DeckSnapshot, runtime: &Handle) {
        self.artwork.refresh(
            deck.current_track_index
                .and_then(|index| deck.tracks.get(index))
                .and_then(|track| track.metadata().artwork.as_ref()),
            runtime,
        );
        self.tempo = format_tempo(deck.speed);
        self.bpm = format_bpm(deck.analysis.bpm, deck.speed);
        self.remain = format_remain(deck);
        self.subtitle = track_subtitle(deck);
        self.quality = format_quality(deck);
        self.refresh_wave(deck.analysis.artifacts.as_ref());
    }

    fn refresh_wave(&mut self, analysis: Option<&TrackArtifacts>) {
        let wave = analysis.and_then(TrackArtifacts::waveform);
        let src = analysis.and_then(TrackArtifacts::waveform_id);
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

fn format_quality(deck: &DeckSnapshot) -> String {
    let stream = &deck.stream;
    let rung = stream.current.or(stream.selected).and_then(|index| {
        stream
            .variants
            .iter()
            .find(|variant| variant.index == index)
    });
    match (stream.is_auto, rung) {
        (true, Some(rung)) => format!("AUTO\u{b7}{}", rung.label),
        (true, None) => "AUTO".to_owned(),
        (false, Some(rung)) => rung.label.clone(),
        (false, None) => String::new(),
    }
}

fn format_tempo(speed: f32) -> String {
    let tenths = (f32::from(TempoPercent::at_speed(speed)) * 10.0).round();
    // Adding zero turns a rounded -0.0 into 0.0.
    format!("{:+.1}%", tenths / 10.0 + 0.0)
}

fn format_bpm(source: Option<f32>, speed: f32) -> String {
    const EM_DASH: char = '\u{2014}';

    source.map_or_else(|| EM_DASH.to_string(), |bpm| format!("{:.1}", bpm * speed))
}

fn format_remain(deck: &DeckSnapshot) -> String {
    const MINUS_SIGN: char = '\u{2212}';

    let left = (deck.duration - deck.position).max(0.0);
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
    use std::num::NonZeroU32;

    use ::kithara::{
        abr::AbrMode,
        platform::{
            CancelToken,
            sync::{Arc, Mutex},
            time::Duration,
            tokio::{sync::watch, task},
        },
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        analysis::{AnalysisHandle, fixtures},
        mix::MixStrip,
        state::{AbrVariant, UiState, listen},
        waveform::TrackAnalysis,
    };

    #[kithara::test(native, tokio)]
    async fn a_revision_offered_to_an_entry_redraws_the_deck_waveform() {
        let cancel = CancelToken::root();
        let (host, queue) = fixtures::queue_off().await;
        let (track_id, source) = fixtures::track(&host, 1, "file:///tmp/track-1.mp3").await;
        let config = fixtures::app_config(&cancel, fixtures::memory_store());
        let mut entry = fixtures::entry(&config, queue.clone(), track_id, source);
        let state = Arc::new(Mutex::new(UiState::new(&queue)));
        let (analysis, mut requests) = AnalysisHandle::channel(watch::channel(Arc::default()).1);
        task::spawn(listen(
            queue.clone(),
            Arc::clone(&state),
            cancel.clone(),
            queue.subscribe(),
            analysis,
            fixtures::grid_post(&host).await,
        ));
        let (asked, reply) = fixtures::next_subscribe(&mut requests).await;
        assert_eq!(asked, track_id);
        assert!(reply.send(entry.subscribe()).is_ok());
        let mut cache = DeckCache::default();
        cache.refresh_wave(state.lock().analysis.as_ref());
        let before = cache.wave_revision;

        let published = fixtures::snapshot("track".into(), 1, 1_000, fixtures::fingerprint(), None);
        assert!(entry.offer(fixtures::progress(published)));
        fixtures::wait_for_revision(&state, 1).await;

        cache.refresh_wave(state.lock().analysis.as_ref());
        assert_ne!(
            cache.wave_revision, before,
            "the deck draws the revision the entry published"
        );
        cancel.cancel();
        host.close().await;
    }

    fn wave_of(height: u8) -> Waveform {
        let blob = [
            1, 0, 0, 0, 0, 0, 0, height, 0, 0, 0, height, 0, 0, 0, height,
        ];
        Waveform::try_from(blob.as_slice()).expect("hand-built blob is valid")
    }

    fn revision(revision: u64, wave: Waveform) -> TrackArtifacts {
        TrackAnalysis::builder()
            .media_origin(Duration::ZERO)
            .token("fixture".into())
            .revision(revision)
            .source_sample_rate(NonZeroU32::new(44_100).expect("fixture rate is non-zero"))
            .waveform(wave)
            .build()
            .into()
    }

    #[kithara::test]
    fn the_deck_names_the_run_of_buckets_it_just_wrote() {
        let mut cache = DeckCache::default();
        let quiet = revision(1, wave_of(62));

        cache.refresh_wave(Some(&quiet));
        let named = cache.wave_revision;

        cache.refresh_wave(Some(&quiet));
        assert_eq!(cache.wave_revision, named, "nothing was written");

        cache.refresh_wave(Some(&revision(2, wave_of(63))));
        assert_ne!(
            cache.wave_revision, named,
            "a new run of buckets was written"
        );
    }

    #[kithara::test]
    fn the_deck_cache_follows_the_revision_not_the_bucket_address() {
        let mut cache = DeckCache::default();
        let wave = wave_of(62);

        cache.refresh_wave(Some(&revision(1, wave.clone())));
        let named = cache.wave_revision;

        cache.refresh_wave(Some(&revision(2, wave)));
        assert_ne!(
            cache.wave_revision, named,
            "the same bucket store under a new revision is a new run"
        );
    }

    #[kithara::test]
    fn a_speed_just_under_one_reads_as_no_tempo_change() {
        assert_eq!(format_tempo(1.0 - f32::EPSILON), "+0.0%");
        assert_eq!(format_tempo(1.05), "+5.0%");
    }

    #[kithara::test]
    fn dropping_the_track_names_the_empty_run() {
        let mut cache = DeckCache::default();
        cache.refresh_wave(Some(&revision(1, wave_of(63))));
        let named = cache.wave_revision;

        cache.refresh_wave(None);

        assert!(cache.wave.is_empty());
        assert_ne!(cache.wave_revision, named, "the empty run is a run too");
    }

    fn shown(ui: &UiState) -> DeckSnapshot {
        DeckSnapshot::new(
            ::kithara::warp::BeatGridId::allocate().expect("a fixture deck id"),
            "a",
            MixStrip::new(0, 1),
            Vec::new(),
            TempoPercent::DEFAULT,
            false,
            ui,
        )
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

        assert_eq!(format_quality(&shown(&ui)), "AUTO\u{b7}320k");

        ui.abr_mode = Some(AbrMode::manual(0));
        ui.current_variant = Some(0);

        assert_eq!(format_quality(&shown(&ui)), "128k");
    }

    #[kithara::test]
    fn a_stream_with_no_rung_yet_still_reports_its_mode() {
        let ui = UiState::empty();

        assert_eq!(format_quality(&shown(&ui)), "AUTO");
    }

    #[kithara::test]
    fn a_layout_answers_to_the_deck_count_the_menu_names() {
        for layout in [DeckLayout::Single, DeckLayout::Dual] {
            assert_eq!(DeckLayout::from_decks(layout.letters().len()), Some(layout));
        }

        assert_eq!(DeckLayout::from_decks(3), None);
    }
}
