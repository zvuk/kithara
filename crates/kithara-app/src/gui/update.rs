use iced::Task;
use tracing::error;

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    message::Message,
    mix,
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
        Message::Tick => {
            handle_tick(state);
            Task::none()
        }
        Message::WindowCloseRequested => iced::exit(),
    };

    refresh_snapshots(state);
    task
}

fn handle_deck(state: &mut Kithara, id: DeckId, msg: &DeckMsg) {
    if let Some(target) = state.decks.get_mut(id) {
        deck::handle(target, msg);
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
}

/// One consistent snapshot per deck per frame, taken after the update.
fn refresh_snapshots(state: &mut Kithara) {
    for deck in state.decks.iter_mut() {
        deck.ui = deck.controller.snapshot();
    }
}
