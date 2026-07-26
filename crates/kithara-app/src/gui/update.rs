use iced::{Task, window};
use tracing::error;

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    message::Message,
    mix, shot, studio_ui,
};
use crate::{catalog, deck::DeckId};
use kithara_ui::render::WindowCommand;

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::Ui(event) => {
            if let Some(translated) = studio_ui::translate(state, event) {
                return update(state, translated);
            }
            Task::none()
        }
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
        Message::UnloadFromDeck(index, id) => {
            handle_unload(state, index, id);
            Task::none()
        }
        Message::PauseHiddenDecks => {
            pause_hidden_decks(state);
            Task::none()
        }
        Message::Tick => {
            handle_tick(state);
            shot::drive(state)
        }
        Message::Window(command) => window_task(state, command),
        Message::Shot(screenshot) => shot::save(state, &screenshot),
        Message::WindowCloseRequested => iced::exit(),
    };

    refresh_snapshots(state);
    task
}

/// The studio draws its own window chrome, so the app executes what the bar
/// asks against the window it opened.
fn window_task(state: &Kithara, command: WindowCommand) -> Task<Message> {
    match command {
        WindowCommand::Drag => window::drag(state.window_id),
        WindowCommand::Minimize => window::minimize(state.window_id, true),
        WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
        WindowCommand::Close => window::close(state.window_id),
        _ => Task::none(),
    }
}

/// A deck the studio no longer lays out keeps its queue but stops playing.
fn pause_hidden_decks(state: &mut Kithara) {
    let hidden: Vec<DeckId> = state
        .session
        .decks()
        .iter()
        .skip(state.studio.cache.layout.decks())
        .map(|deck| deck.id)
        .collect();
    for id in hidden {
        handle_deck(state, id, &DeckMsg::Pause);
    }
}

fn handle_deck(state: &mut Kithara, id: DeckId, msg: &DeckMsg) {
    if let Some(target) = state.decks.get_mut(id) {
        deck::handle(target, msg);
    }
}

/// Clicking a row highlights it; loading goes through the row's A/B assign
/// chips so the target deck is always explicit.
fn handle_select_catalog(state: &mut Kithara, index: usize) {
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

fn handle_unload(state: &mut Kithara, index: usize, id: DeckId) {
    let Some(entry) = state.catalog.get(index) else {
        return;
    };
    let Some(deck) = state.decks.get(id) else {
        return;
    };
    if let Err(e) = catalog::unload_from(deck.controller.queue(), entry) {
        error!(index, deck = id.0, error = %e, "unload from deck failed");
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

/// One consistent snapshot per deck per frame, taken after the update. The
/// studio cache re-derives its renderer-borrowed state from the snapshots.
fn refresh_snapshots(state: &mut Kithara) {
    for deck in state.decks.iter_mut() {
        deck.ui = deck.controller.snapshot();
    }
    state.studio.cache.refresh(&state.decks, &state.catalog);
}
