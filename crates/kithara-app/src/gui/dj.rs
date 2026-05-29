use iced::{Task, window};

use super::{app::Kithara, frontend::window_settings, message::Message};

/// View-local DJ Studio state. Thin by design: waveform analysis is owned
/// by [`crate::state::StateController`], so the studio only needs to know
/// whether it is currently open.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DjView {
    pub(crate) open: bool,
}

/// DJ Studio control events, grouped so the top-level [`Message`] stays thin.
#[derive(Debug, Clone)]
pub(crate) enum DjMsg {
    /// Toggle between the compact player and the DJ Studio shell. Opens a
    /// fresh window for the target mode and closes the previous one.
    Toggle,
}

pub(crate) fn handle(state: &mut Kithara, msg: &DjMsg) -> Task<Message> {
    match msg {
        DjMsg::Toggle => handle_toggle(state),
    }
}

/// Swap the live window for the other mode. The new window opens before
/// the old one closes so the daemon always has a window in flight.
fn handle_toggle(state: &mut Kithara) -> Task<Message> {
    state.dj.open = !state.dj.open;
    state.controller.set_waveform_enabled(state.dj.open);

    let old = state.window_id;
    let (new_id, open) = window::open(window_settings(state.dj.open));
    state.window_id = Some(new_id);
    let close_old = old.map_or_else(Task::none, window::close);
    open.discard().chain(close_old)
}
