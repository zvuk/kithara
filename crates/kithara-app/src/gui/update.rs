use iced::{Task, window};
use tracing::error;

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    frontend::window_settings,
    message::Message,
    mix, url_bar,
};
use crate::{catalog, deck::DeckId};

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::Deck(id, msg) => {
            handle_deck(state, id, &msg);
            Task::none()
        }
        Message::Mix(msg) => {
            mix::handle(state, msg);
            Task::none()
        }
        Message::Url(msg) => url_bar::handle(state, msg),
        Message::TabSelected(tab) => {
            state.active_tab = tab;
            Task::none()
        }
        Message::DeleteFocusedTrack => {
            handle_deck(state, state.decks.focus(), &DeckMsg::DeleteTrack);
            Task::none()
        }
        Message::SelectCatalogTrack(index) => {
            handle_select_catalog(state, index);
            Task::none()
        }
        Message::LoadOntoDeck(index, id) => {
            handle_load(state, index, id);
            Task::none()
        }
        Message::FocusDeck(id) => {
            state.decks.set_focus(id);
            Task::none()
        }
        Message::ToggleStudio => handle_toggle_studio(state),
        Message::Tick => {
            handle_tick(state);
            Task::none()
        }
        Message::WindowCloseRequested(id) => handle_window_close_requested(state, id),
    };

    refresh_snapshots(state);
    task
}

fn handle_deck(state: &mut Kithara, id: DeckId, msg: &DeckMsg) {
    if let Some(target) = state.decks.get_mut(id) {
        deck::handle(target, msg);
    }
}

/// Swap the live window for the other mode. The new window opens before
/// the old one closes so the daemon always has a window in flight.
fn handle_toggle_studio(state: &mut Kithara) -> Task<Message> {
    state.studio_open = !state.studio_open;

    let old = state.window_id;
    let (new_id, open) = window::open(window_settings(state.studio_open));
    state.window_id = Some(new_id);
    let close_old = old.map_or_else(Task::none, window::close);
    open.discard().chain(close_old)
}

fn handle_window_close_requested(state: &Kithara, id: window::Id) -> Task<Message> {
    if state.window_id == Some(id) {
        iced::exit()
    } else {
        window::close(id)
    }
}

/// First click highlights the row; a second click on the same row loads it
/// onto the focused deck. Matches the deck playlist behaviour.
fn handle_select_catalog(state: &mut Kithara, index: usize) {
    if state.selected_track == Some(index) {
        handle_load(state, index, state.decks.focus());
        return;
    }
    state.selected_track = Some(index);
}

fn handle_load(state: &mut Kithara, index: usize, id: DeckId) {
    let Some(entry) = state.catalog.get(index) else {
        return;
    };
    let Some(deck) = state.decks.get(id) else {
        return;
    };
    if let Err(e) = catalog::load_onto(deck.controller.queue(), entry, &state.config) {
        error!(index, deck = id.0, error = %e, "load onto deck failed");
    }
}

/// Every deck advances on the same tick: a deck the user is not looking at
/// still plays, streams and needs its continuous values pulled.
fn handle_tick(state: &mut Kithara) {
    for deck in state.decks.iter() {
        let _ = deck.controller.queue().tick();
        deck.controller.refresh_continuous();
    }
    state.blink_counter = state.blink_counter.wrapping_add(1);
}

/// One consistent snapshot per deck per frame, taken after the update.
fn refresh_snapshots(state: &mut Kithara) {
    for deck in state.decks.iter_mut() {
        deck.ui = deck.controller.snapshot();
    }
}
