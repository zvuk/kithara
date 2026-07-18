use iced::{Task, window};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara::events::{DjEvent, StretchBackendKind};
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
use kithara::prelude::StretchKind;

use super::{
    app::Kithara, frontend::window_settings, message::Message, modular::ViewMode, widgets::Viewport,
};
use crate::gui::widgets::WaveMsg;

/// View-local DJ Studio state.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DjView {
    pub(crate) timestretch: TimestretchState,
    pub(crate) wave: Viewport,
    pub(crate) open: bool,
}

/// View-local timestretch deck state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimestretchState {
    /// Tempo offset in percent.
    pub(crate) tempo: f32,
    /// Tempo bound in ± percent (8 / 16 / 50 / 100).
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

    /// Playback speed multiplier for the current tempo offset.
    pub(crate) fn speed(self) -> f32 {
        1.0 + self.tempo / 100.0
    }
}

/// DJ Studio control events.
#[derive(Debug, Clone)]
pub(crate) enum DjMsg {
    /// Toggle between the compact player and the DJ Studio shell.
    Toggle,
    /// Deck waveform zoom/pan request (wheel, drag, or the `−`/`+` control).
    Wave(WaveMsg),
    /// Set the tempo offset (± percent) from the slider.
    SetTempo(f32),
    /// Select a tempo range bound (± percent).
    SetRange(u8),
    /// Nudge the tempo by a small delta (± percent).
    Nudge(f32),
    /// Reset tempo to 0 %.
    ResetTempo,
    /// Toggle key-lock (pitch-preserving tempo).
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    ToggleKeyLock,
    /// Select the time-stretch backend.
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    SelectBackend(StretchKind),
}

pub(crate) fn handle(state: &mut Kithara, msg: &DjMsg) -> Task<Message> {
    match msg {
        DjMsg::Toggle => return handle_toggle(state),
        DjMsg::Wave(m) => {
            state.dj.wave = state.dj.wave.apply(*m);
            return Task::none();
        }
        DjMsg::SetTempo(t) => state.dj.timestretch.set_tempo(*t),
        DjMsg::SetRange(r) => state.dj.timestretch.set_range(*r),
        DjMsg::Nudge(d) => state.dj.timestretch.nudge(*d),
        DjMsg::ResetTempo => state.dj.timestretch.tempo = 0.0,
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        DjMsg::ToggleKeyLock => {
            // Applies live, mid-track (shared controls read each chunk).
            let deck = state.controller.deck();
            deck.set_keylock(!deck.keylock());
            state
                .controller
                .queue()
                .bus()
                .publish(DjEvent::KeylockChanged { on: deck.keylock() });
        }
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        DjMsg::SelectBackend(backend) => {
            // Applies live, mid-track (shared controls read each chunk).
            state.controller.deck().set_backend(*backend);
            state
                .controller
                .queue()
                .bus()
                .publish(DjEvent::StretchBackendChanged {
                    kind: stretch_backend_kind(*backend),
                });
        }
    }

    // Live tempo: mirror the speed to the queue.
    state
        .controller
        .queue()
        .set_rate(state.dj.timestretch.speed());
    Task::none()
}

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
fn stretch_backend_kind(kind: StretchKind) -> StretchBackendKind {
    match kind {
        StretchKind::Signalsmith => StretchBackendKind::Signalsmith,
        _ => StretchBackendKind::Unknown,
    }
}

/// Swap the live window for the other mode. The new window opens before
/// the old one closes so the daemon always has a window in flight.
fn handle_toggle(state: &mut Kithara) -> Task<Message> {
    state.dj.open = !state.dj.open;
    state.view_mode = if state.dj.open {
        ViewMode::Studio
    } else {
        ViewMode::Compact
    };

    let old = state.window_id;
    let (new_id, open) = window::open(window_settings(state.dj.open));
    state.window_id = Some(new_id);
    let close_old = old.map_or_else(Task::none, window::close);
    open.discard().chain(close_old)
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
