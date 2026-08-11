use iced::{Task, window, window::Direction};
use kithara::audio::EqBandConfig;
use kithara_ui::render::{WindowCommand, WindowEdge};
use tracing::{error, warn};

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    message::Message,
    mix, studio_ui, transport,
};
use crate::{
    catalog,
    deck::{DeckId, EqMode},
    state::StateController,
};

struct EqModeChange<'a> {
    id: DeckId,
    controller: &'a StateController,
    previous: Vec<EqBandConfig>,
    next: Vec<EqBandConfig>,
    gains: Vec<f32>,
}

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::BroadcastToggle => state
            .broadcast
            .toggle()
            .map_or_else(Task::none, stop_broadcast),
        Message::BroadcastStopped(duration) => {
            state.broadcast.complete_stop();
            if let Some(duration) = duration {
                tracing::info!(
                    elapsed_ms = duration.as_secs_f64() * 1_000.0,
                    "broadcast stopped"
                );
            }
            Task::none()
        }
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
        Message::SetEqMode(mode) => {
            set_eq_mode(state, mode);
            Task::none()
        }
        Message::Mix(msg) => {
            mix::handle(state, msg);
            Task::none()
        }
        Message::Transport(msg) => {
            transport::handle(state, msg);
            Task::none()
        }
        Message::DeleteFocusedTrack => {
            delete_focused_track(state);
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
        Message::PauseHiddenDecks => {
            pause_hidden_decks(state);
            Task::none()
        }
        Message::Tick => {
            handle_tick(state);
            Task::none()
        }
        Message::Window(command) => window_task(state, command),
        Message::WindowCloseRequested => iced::exit(),
    };

    refresh_snapshots(state);
    task
}

fn stop_broadcast(stop: crate::broadcast::BroadcastStop) -> Task<Message> {
    Task::perform(stop.run(), Message::BroadcastStopped)
}

/// The studio draws its own window chrome, so the app executes what the bar
/// asks against the window it opened.
fn window_task(state: &Kithara, command: WindowCommand) -> Task<Message> {
    match command {
        WindowCommand::Drag => window::drag(state.window_id),
        WindowCommand::Resize(edge) => direction(edge).map_or_else(Task::none, |direction| {
            window::drag_resize(state.window_id, direction)
        }),
        WindowCommand::Minimize => window::minimize(state.window_id, true),
        WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
        WindowCommand::Close => iced::exit(),
        other => {
            warn!(?other, "unhandled window command");
            Task::none()
        }
    }
}

const fn direction(edge: WindowEdge) -> Option<Direction> {
    Some(match edge {
        WindowEdge::North => Direction::North,
        WindowEdge::South => Direction::South,
        WindowEdge::East => Direction::East,
        WindowEdge::West => Direction::West,
        WindowEdge::NorthEast => Direction::NorthEast,
        WindowEdge::NorthWest => Direction::NorthWest,
        WindowEdge::SouthEast => Direction::SouthEast,
        WindowEdge::SouthWest => Direction::SouthWest,
        _ => return None,
    })
}

/// A deck the studio no longer lays out keeps its queue but stops playing.
fn pause_hidden_decks(state: &mut Kithara) {
    let hidden: Vec<DeckId> = state
        .session
        .decks()
        .iter()
        .skip(state.studio.cache.laid_out_decks())
        .map(|deck| deck.id)
        .collect();
    for id in hidden {
        handle_deck(state, id, &DeckMsg::Pause);
    }
}

fn delete_focused_track(state: &mut Kithara) {
    let focus = state.studio.cache.focus_deck();
    let Some(id) = state.session.decks().get(focus).map(|deck| deck.id) else {
        return;
    };
    handle_deck(state, id, &DeckMsg::DeleteTrack);
}

fn handle_deck(state: &mut Kithara, id: DeckId, msg: &DeckMsg) {
    if let Some(target) = state.decks.get_mut(id) {
        deck::handle(target, msg);
    }
}

fn set_eq_mode(state: &mut Kithara, mode: EqMode) {
    let current_mode = state.eq_mode;
    if current_mode == mode {
        return;
    }

    let mut changes = Vec::new();
    for deck in state.decks.iter() {
        let current = deck
            .controller
            .mutate(|deck_state| deck_state.eq_bands.clone());
        let Some(gains) = current_mode.remap(mode, &current) else {
            error!(
                deck = deck.id.0,
                current = ?current_mode,
                requested = ?mode,
                bands = current.len(),
                "EQ mode state does not match its band layout"
            );
            return;
        };
        changes.push(EqModeChange {
            id: deck.id,
            controller: deck.controller.as_ref(),
            previous: current_mode.layout(&current),
            next: mode.layout(&gains),
            gains,
        });
    }

    for (applied, change) in changes.iter().enumerate() {
        if let Err(err) = change.controller.queue().set_eq_layout(change.next.clone()) {
            error!(
                deck = change.id.0,
                requested = ?mode,
                error = ?err,
                "set shared EQ layout failed"
            );
            rollback_eq_mode(&changes[..applied]);
            return;
        }
    }

    for change in changes {
        change
            .controller
            .mutate(|deck_state| deck_state.eq_bands = change.gains);
    }
    state.eq_mode = mode;
}

fn rollback_eq_mode(changes: &[EqModeChange<'_>]) {
    for change in changes.iter().rev() {
        if let Err(err) = change
            .controller
            .queue()
            .set_eq_layout(change.previous.clone())
        {
            error!(
                deck = change.id.0,
                error = ?err,
                "rollback shared EQ layout failed"
            );
        }
    }
}

/// Clicking a row highlights it; a deck gets the row by dragging it there, so
/// the target deck is always the one the pointer chose.
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

/// Every deck advances on the same tick: a deck the user is not looking at
/// still plays, streams and needs its continuous values pulled.
fn handle_tick(state: &mut Kithara) {
    state.broadcast.poll();
    for deck in state.decks.iter() {
        let _ = deck.controller.queue().tick();
        deck.controller.refresh_continuous();
    }
    transport::tick(state);
}

/// One consistent snapshot per deck per frame, taken after the update. The
/// studio cache re-derives its renderer-borrowed state from the snapshots.
fn refresh_snapshots(state: &mut Kithara) {
    for deck in state.decks.iter_mut() {
        deck.ui = deck.controller.snapshot();
    }
    state.studio.cache.refresh(&state.decks, &state.catalog);
}
