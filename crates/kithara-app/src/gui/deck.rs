#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara::prelude::StretchKind;
use kithara_platform::sync::Arc;
use kithara_queue::{TrackId, Transition};
use tracing::{debug, error};

use super::widgets::{Viewport, WaveMsg};
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
    pub(crate) wave: Viewport,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimestretchState {
    pub(crate) tempo: f32,
    pub(crate) range: u8,
}

impl Default for TimestretchState {
    fn default() -> Self {
        Self {
            range: 16,
            tempo: 0.0,
        }
    }
}

impl TimestretchState {
    fn clamp_tempo(&mut self) {
        let r = f32::from(self.range);
        self.tempo = self.tempo.clamp(-r, r);
    }

    fn nudge(&mut self, delta: f32) {
        self.tempo = ((self.tempo + delta) * 100.0).round() / 100.0;
        self.clamp_tempo();
    }

    fn set_range(&mut self, range: u8) {
        self.range = range;
        self.clamp_tempo();
    }

    fn set_tempo(&mut self, tempo: f32) {
        self.tempo = tempo;
        self.clamp_tempo();
    }

    pub(crate) fn speed(self) -> f32 {
        1.0 + self.tempo / 100.0
    }
}

/// Everything a single deck can be told to do. Carries no deck identity: the
/// composer that renders a deck maps this into `Message::Deck(id, msg)`.
#[derive(Debug, Clone)]
pub(crate) enum DeckMsg {
    TogglePlayPause,
    Next,
    Prev,
    SeekTo(f64),
    EqBandChanged(usize, f32),
    DeleteTrack,
    Wave(WaveMsg),
    SetTempo(f32),
    SetRange(u8),
    Nudge(f32),
    ResetTempo,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    ToggleKeyLock,
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    SelectBackend(StretchKind),
}

/// Apply a deck message to its own deck. Nothing here reaches another deck.
pub(crate) fn handle(deck: &mut DeckUi, msg: &DeckMsg) {
    match *msg {
        DeckMsg::TogglePlayPause => toggle_play_pause(deck),
        DeckMsg::Next => {
            deck.controller
                .queue()
                .advance_to_next(Transition::Crossfade);
        }
        DeckMsg::Prev => {
            deck.controller
                .queue()
                .return_to_previous(Transition::Crossfade);
        }
        DeckMsg::SeekTo(pos) => seek_to(deck, pos),
        DeckMsg::EqBandChanged(band, db) => eq_band_changed(deck, band, db),
        DeckMsg::DeleteTrack => delete_track(deck),
        DeckMsg::Wave(m) => deck.view.wave = deck.view.wave.apply(m),
        DeckMsg::SetTempo(t) => set_speed(deck, |ts| ts.set_tempo(t)),
        DeckMsg::SetRange(r) => set_speed(deck, |ts| ts.set_range(r)),
        DeckMsg::Nudge(d) => set_speed(deck, |ts| ts.nudge(d)),
        DeckMsg::ResetTempo => set_speed(deck, |ts| ts.tempo = 0.0),
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        DeckMsg::ToggleKeyLock => {
            // Applies live, mid-track (shared controls read each chunk).
            let stretch = deck.controller.stretch();
            stretch.set_keylock(!stretch.keylock());
        }
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        DeckMsg::SelectBackend(backend) => deck.controller.stretch().set_backend(backend),
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

/// Live tempo: mirror the speed to this deck's queue.
fn set_speed(deck: &mut DeckUi, edit: impl FnOnce(&mut TimestretchState)) {
    edit(&mut deck.view.timestretch);
    deck.controller
        .queue()
        .set_rate(deck.view.timestretch.speed());
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::TimestretchState;

    #[kithara::test]
    fn speed_is_raw_passthrough_floor_owned_by_engine() {
        let ts = TimestretchState {
            range: 100,
            tempo: -100.0,
        };

        assert!(ts.speed().abs() < f32::EPSILON);
    }
}
