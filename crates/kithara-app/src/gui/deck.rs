use kithara_platform::sync::Arc;
use kithara_queue::{TrackId, Transition};
use tracing::{debug, error};

use crate::{
    deck::DeckId,
    state::{StateController, UiState},
};

/// One deck as the GUI sees it: the shared model behind it, the snapshot the
/// current frame renders from, and the view-local state that belongs to no
/// one else.
pub(crate) struct DeckUi {
    pub(crate) id: DeckId,
    pub(crate) controller: Arc<StateController>,
    pub(crate) ui: UiState,
    pub(crate) view: DeckView,
}

impl DeckUi {
    pub(crate) fn new(id: DeckId, controller: Arc<StateController>) -> Self {
        let ui = controller.snapshot();
        Self {
            id,
            controller,
            ui,
            view: DeckView::default(),
        }
    }
}

/// View-local deck state that has no business in the shared model.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeckView {
    pub(crate) timestretch: TimestretchState,
}

/// Tempo travel either way, in percent: tempo spans `-TEMPO_RANGE` to
/// `+TEMPO_RANGE`.
pub(crate) const TEMPO_RANGE: f32 = 50.0;

/// What one wheel detent over the TEMPO block is worth, in percent.
pub(crate) const TEMPO_STEP: f32 = 1.5;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TimestretchState {
    pub(crate) tempo: f32,
}

impl TimestretchState {
    pub(crate) fn speed(self) -> f32 {
        1.0 + self.tempo / 100.0
    }
}

/// Everything a single deck can be told to do. Carries no deck identity: the
/// composer that renders a deck maps this into `Message::Deck(id, msg)`.
#[derive(Debug, Clone)]
pub(crate) enum DeckMsg {
    TogglePlayPause,
    Pause,
    Next,
    Prev,
    SeekTo(f64),
    EqBandChanged(usize, f32),
    DeleteTrack,
    SetTempo(f32),
}

/// Apply a deck message to its own deck. Nothing here reaches another deck.
pub(crate) fn handle(deck: &mut DeckUi, msg: &DeckMsg) {
    match *msg {
        DeckMsg::TogglePlayPause => toggle_play_pause(deck),
        DeckMsg::Pause => deck.controller.queue().pause(),
        DeckMsg::Next => {
            deck.controller.queue().advance_to_next(
                Transition::Crossfade,
                kithara::events::AdvanceReason::UserNext,
            );
        }
        DeckMsg::Prev => {
            deck.controller
                .queue()
                .return_to_previous(Transition::Crossfade);
        }
        DeckMsg::SeekTo(pos) => seek_to(deck, pos),
        DeckMsg::EqBandChanged(band, db) => eq_band_changed(deck, band, db),
        DeckMsg::DeleteTrack => delete_track(deck),
        DeckMsg::SetTempo(tempo) => set_tempo(deck, tempo),
    }
}

fn toggle_play_pause(deck: &DeckUi) {
    if deck.ui.playing {
        deck.controller.queue().pause();
    } else {
        deck.controller.queue().play();
    }
}

fn seek_to(deck: &DeckUi, pos: f64) {
    deck.controller.mutate(|st| {
        st.is_seeking = false;
        st.seek_position = pos;
    });
    seek(deck, pos);
}

fn seek(deck: &DeckUi, target: f64) {
    if let Err(e) = deck.controller.queue().seek(target) {
        error!("seek failed: {e:?}");
    }
}

fn eq_band_changed(deck: &DeckUi, band: usize, db: f32) {
    if band >= deck.ui.eq_bands.len() {
        return;
    }
    // `eq_bands` is the user's desired EQ and the source of truth: record it
    // regardless of whether a playback slot exists yet. The listener re-applies
    // it to the engine once a track becomes active.
    deck.controller.mutate(|st| {
        if let Some(slot) = st.eq_bands.get_mut(band) {
            *slot = db;
        }
    });
    if let Err(e) = deck.controller.queue().set_eq_gain(band, db) {
        // Expected before playback starts (no active slot yet); the gain is
        // retained in `eq_bands` and pushed down when playback begins.
        debug!("set EQ gain band={band} db={db:.1} deferred: {e:?}");
    }
}

fn track_id_at(deck: &DeckUi, index: usize) -> Option<TrackId> {
    deck.ui.tracks.get(index).map(|e| e.id)
}

fn delete_track(deck: &mut DeckUi) {
    if let Some(idx) = deck.ui.current_track_index
        && let Some(id) = track_id_at(deck, idx)
        && let Err(e) = deck.controller.queue().remove(id)
    {
        error!(index = idx, error = %e, "remove failed");
    }
}

/// Live tempo: clamp to the travel and mirror the speed to this deck's queue.
fn set_tempo(deck: &mut DeckUi, tempo: f32) {
    let timestretch = &mut deck.view.timestretch;
    timestretch.tempo = tempo.clamp(-TEMPO_RANGE, TEMPO_RANGE);
    deck.controller.queue().set_rate(timestretch.speed());
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{TEMPO_RANGE, TEMPO_STEP, TimestretchState};

    #[kithara::test]
    fn speed_is_one_percent_per_tempo_point() {
        let speed = |tempo| TimestretchState { tempo }.speed();

        assert!((speed(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((speed(TEMPO_RANGE) - 1.5).abs() < 1e-6);
        assert!((speed(-TEMPO_RANGE) - 0.5).abs() < 1e-6);
    }

    #[kithara::test]
    fn the_whole_travel_is_within_reach_of_a_few_detents() {
        const REACH: f32 = 40.0;

        let detents = TEMPO_RANGE / TEMPO_STEP;
        assert!(
            detents <= REACH,
            "one end of the travel takes {detents} detents"
        );
    }
}
