use kithara::{
    abr::AbrMode, effects::GainDb, platform::sync::Arc, prelude::EngineLoadSnapshot,
    queue::TrackEntry,
};
use num_traits::cast::AsPrimitive;

use crate::{
    analysis::TrackArtifacts,
    broadcast::Broadcaster,
    deck::{DeckId, EqMode, TempoPercent},
    engine::settings::DeckSettings,
    mix::MixState,
    state::{AbrVariant, UiState},
};

#[derive(Clone)]
pub(crate) struct EngineSnapshot {
    pub(crate) broadcast: BroadcastPhase,
    pub(crate) eq_mode: EqMode,
    pub(crate) mix: MixState,
    pub(crate) decks: Vec<DeckSnapshot>,
    pub(crate) applied_seq: u64,
}

impl EngineSnapshot {
    pub(crate) fn unpublished() -> Self {
        Self {
            broadcast: BroadcastPhase::default(),
            eq_mode: EqMode::default(),
            mix: MixState::new(0),
            decks: Vec::new(),
            applied_seq: 0,
        }
    }

    pub(crate) fn deck(&self, id: DeckId) -> Option<&DeckSnapshot> {
        self.decks.iter().find(|deck| deck.id == id)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BroadcastPhase {
    pub(crate) url: String,
    pub(crate) is_on_air: bool,
}

impl BroadcastPhase {
    pub(crate) fn new(broadcaster: &Broadcaster) -> Self {
        Self {
            url: broadcaster.url().unwrap_or_default().to_owned(),
            is_on_air: broadcaster.is_on_air(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DeckSnapshot {
    pub(crate) analysis: AnalysisView,
    pub(crate) eq_bands: Vec<GainDb>,
    pub(crate) stream: StreamView,
    pub(crate) track_name: String,
    pub(crate) tracks: Vec<TrackEntry>,
    pub(crate) id: DeckId,
    pub(crate) engine_load: EngineLoadSnapshot,
    pub(crate) current_track_index: Option<usize>,
    pub(crate) tempo: TempoPercent,
    pub(crate) playing: bool,
    pub(crate) duration: f64,
    pub(crate) position: f64,
}

#[derive(Clone)]
pub(crate) struct StreamView {
    pub(crate) current: Option<usize>,
    pub(crate) selected: Option<usize>,
    pub(crate) variants: Vec<AbrVariant>,
    pub(crate) is_auto: bool,
}

#[derive(Clone)]
pub(crate) struct AnalysisView {
    pub(crate) beats: Arc<[f32]>,
    pub(crate) downbeats: Arc<[f32]>,
    pub(crate) unready: Arc<[[f32; 2]]>,
    pub(crate) artifacts: Option<TrackArtifacts>,
    pub(crate) bpm: Option<f32>,
}

impl DeckSnapshot {
    pub(crate) fn new(id: DeckId, state: &UiState, settings: &DeckSettings) -> Self {
        Self {
            id,
            analysis: AnalysisView::new(state),
            eq_bands: settings.eq_bands.clone(),
            stream: StreamView::new(state),
            track_name: state.track_name.clone(),
            tracks: state.tracks.clone(),
            engine_load: state.engine_load,
            tempo: settings.tempo,
            current_track_index: state.current_track_index,
            duration: state.duration,
            position: state.position,
            playing: state.playing,
        }
    }
}

impl StreamView {
    fn new(state: &UiState) -> Self {
        let selected = match state.abr_mode {
            Some(AbrMode::Manual(variant)) => Some(variant.get()),
            Some(AbrMode::Auto(_)) | None => None,
        };
        Self {
            current: state.current_variant,
            selected,
            variants: state.abr_variants.clone(),
            is_auto: selected.is_none(),
        }
    }
}

impl AnalysisView {
    fn new(state: &UiState) -> Self {
        Self {
            artifacts: state.analysis.clone(),
            bpm: state
                .analysis
                .as_ref()
                .and_then(TrackArtifacts::grid)
                .map(|grid| grid.as_raw().bpm.as_()),
            beats: Arc::clone(&state.beat_marks),
            downbeats: Arc::clone(&state.downbeat_marks),
            unready: Arc::clone(&state.unready_ranges),
        }
    }
}
