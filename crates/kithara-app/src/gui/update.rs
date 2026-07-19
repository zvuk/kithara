use iced::{Task, window};
use kithara_queue::{TrackId, Transition};
use kithara_ui::render::UiEvent;
use tracing::error;

use super::{app::Kithara, message::Message, modular};

fn track_id_at(state: &Kithara, index: usize) -> Option<TrackId> {
    state.ui_state.tracks.get(index).map(|entry| entry.id)
}

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::Tick => {
            handle_tick(state);
            Task::none()
        }
        Message::WindowCloseRequested(id) => handle_window_close_requested(state, id),
        Message::DeleteTrack => {
            handle_delete_track(state);
            Task::none()
        }
        Message::Modular(message) => modular::update(state, message),
    };

    state.ui_state = state.controller.snapshot();
    task
}

fn handle_window_close_requested(state: &mut Kithara, id: window::Id) -> Task<Message> {
    if state.settings_window_id == Some(id) {
        modular::update(state, UiEvent::CloseSettings)
    } else if state.window_id == Some(id) {
        iced::exit()
    } else {
        window::close(id)
    }
}

pub(in crate::gui) fn handle_toggle_play_pause(state: &Kithara) {
    if state.ui_state.playing {
        state.controller.queue().pause();
    } else {
        state.controller.queue().play();
    }
}

pub(in crate::gui) fn handle_prev(state: &Kithara) {
    let _ = state
        .controller
        .queue()
        .return_to_previous(Transition::Crossfade);
}

pub(in crate::gui) fn handle_next(state: &Kithara) {
    let _ = state.controller.queue().advance_to_next(
        Transition::Crossfade,
        kithara::events::AdvanceReason::UserNext,
    );
}

pub(in crate::gui) fn handle_seek_to(state: &Kithara, position: f64) {
    state.controller.mutate(|ui| {
        ui.is_seeking = false;
        ui.seek_position = position;
    });

    if let Err(error) = state.controller.queue().seek(position) {
        error!(?error, "seek failed");
    }
}

pub(in crate::gui) fn handle_volume_changed(state: &Kithara, volume: f32) {
    state.controller.queue().set_volume(volume);
    state.controller.mutate(|ui| ui.volume = volume);
}

pub(in crate::gui) fn handle_select_track(state: &mut Kithara, index: usize) {
    if state.selected_track_index != Some(index) {
        state.selected_track_index = Some(index);
        return;
    }
    if let Some(id) = track_id_at(state, index)
        && let Err(error) = state.controller.queue().select(id, Transition::None)
    {
        error!(index, %error, "select failed");
    }
}

fn handle_delete_track(state: &mut Kithara) {
    let target = state
        .selected_track_index
        .or(state.ui_state.current_track_index);
    if let Some(index) = target
        && let Some(id) = track_id_at(state, index)
    {
        match state.controller.queue().remove(id) {
            Ok(()) => state.selected_track_index = None,
            Err(error) => error!(index, %error, "remove failed"),
        }
    }
}

fn handle_tick(state: &Kithara) {
    let _ = state.controller.queue().tick();
    state.controller.refresh_continuous();
}
