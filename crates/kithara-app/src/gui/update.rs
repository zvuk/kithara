use iced::{
    Task, window,
    window::{Direction, Mode},
};
use kithara::{
    platform::time::Duration,
    play::effects::eq::{EqBandConfig, GainDb},
    ui::render::{WindowCommand, WindowEdge},
};
use tracing::{error, warn};

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    message::Message,
    mix,
    subscription::subscription_config,
    ui,
};
use crate::{
    catalog,
    deck::{DeckId, EqMode},
    state::StateController,
};

struct EqModeChange<'a> {
    controller: &'a StateController,
    id: DeckId,
    gains: Vec<GainDb>,
    next: Vec<EqBandConfig>,
    previous: Vec<EqBandConfig>,
}

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        Message::BroadcastToggle => state
            .broadcast
            .toggle(state.session.host())
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
            if let Some(translated) = ui::translate(state, event) {
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
        Message::WindowResized(size) => {
            state.ui.cache.window.set_size(size);
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

/// The app draws its own window chrome, so the app executes what the bar
/// asks against the window it opened.
fn window_task(state: &Kithara, command: WindowCommand) -> Task<Message> {
    match command {
        WindowCommand::Drag => window::drag(state.window_id),
        WindowCommand::Resize(edge) => direction(edge).map_or_else(Task::none, |direction| {
            window::drag_resize(state.window_id, direction)
        }),
        WindowCommand::Minimize => window::minimize(state.window_id, true),
        WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
        WindowCommand::ToggleFullScreen => toggle_full_screen(state.window_id),
        WindowCommand::Close => iced::exit(),
        other => {
            warn!(?other, "unhandled window command");
            Task::none()
        }
    }
}

/// Only the window manager knows the mode the window is in, so the toggle
/// reads it back before asking for the other one.
fn toggle_full_screen(id: window::Id) -> Task<Message> {
    window::mode(id).then(move |mode| {
        let next = if mode == Mode::Fullscreen {
            Mode::Windowed
        } else {
            Mode::Fullscreen
        };
        window::set_mode(id, next)
    })
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

/// A deck the app no longer lays out keeps its queue but stops playing.
fn pause_hidden_decks(state: &mut Kithara) {
    let hidden: Vec<DeckId> = state
        .session
        .decks()
        .iter()
        .skip(state.ui.cache.laid_out_decks())
        .map(|deck| deck.id)
        .collect();
    for id in hidden {
        handle_deck(state, id, &DeckMsg::Pause);
    }
}

fn delete_focused_track(state: &mut Kithara) {
    let focus = state.ui.cache.focus_deck();
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

    let mut changes: Vec<EqModeChange<'_>> = Vec::new();
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
const fn handle_select_catalog(state: &mut Kithara, index: usize) {
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
    let playing = state.decks.iter().any(|deck| deck.ui.playing);
    state.ui.advance(Duration::from_millis(
        subscription_config(playing).tick_interval_ms,
    ));
    state.broadcast.poll(state.session.host());
    for deck in state.decks.iter() {
        let _ = deck.controller.queue().tick();
        deck.controller.refresh_continuous();
    }
}

/// One consistent snapshot per deck per frame, taken after the update. The
/// view cache re-derives its renderer-borrowed state from the snapshots.
fn refresh_snapshots(state: &mut Kithara) {
    for deck in state.decks.iter_mut() {
        deck.ui = deck.controller.snapshot();
    }
    state.ui.cache.refresh(&state.decks, &state.catalog);
}

#[cfg(all(test, not(feature = "broadcast")))]
mod tests {
    use std::mem;

    use ::kithara::{
        play::effects::eq::GainDb,
        ui::render::{ControlAction, UiEvent, WindowCommand, WindowEdge},
    };
    use iced::{Size, window::Direction};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::gui::{test_fixture, ui::cache::DeckLayout};

    fn apply(state: &mut Kithara, message: Message) {
        assert_eq!(update(state, message).units(), 0);
    }

    #[kithara::test(native, flash(false))]
    fn window_edges_keep_their_native_direction() {
        let cases = [
            (WindowEdge::North, Direction::North),
            (WindowEdge::South, Direction::South),
            (WindowEdge::East, Direction::East),
            (WindowEdge::West, Direction::West),
            (WindowEdge::NorthEast, Direction::NorthEast),
            (WindowEdge::NorthWest, Direction::NorthWest),
            (WindowEdge::SouthEast, Direction::SouthEast),
            (WindowEdge::SouthWest, Direction::SouthWest),
        ];

        for (edge, expected) in cases {
            let actual = direction(edge).expect("window edge has a native direction");
            assert_eq!(mem::discriminant(&actual), mem::discriminant(&expected));
        }
    }

    #[kithara::test(native, flash(false))]
    fn every_window_command_schedules_host_work() {
        let state = test_fixture::state();
        let commands = [
            WindowCommand::Drag,
            WindowCommand::Resize(WindowEdge::North),
            WindowCommand::Minimize,
            WindowCommand::ToggleMaximize,
            WindowCommand::ToggleFullScreen,
            WindowCommand::Close,
        ];

        for command in commands {
            assert_eq!(window_task(&state, command).units(), 1, "{command:?}");
        }
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn update_routes_messages_and_refreshes_their_state() {
        let mut state = test_fixture::state();

        assert_eq!(
            update(
                &mut state,
                Message::Ui(UiEvent::Control {
                    path: "mixer/xfade".to_string(),
                    action: ControlAction::SetScalar(1.0),
                }),
            )
            .units(),
            0
        );
        assert_eq!(state.session.mix().position, 1.0);

        assert_eq!(
            update(
                &mut state,
                Message::Ui(UiEvent::LibraryQuery("local".to_string())),
            )
            .units(),
            0
        );
        assert_eq!(state.ui.cache.library.query, "local");

        apply(&mut state, Message::SelectCatalogTrack(1));
        assert_eq!(state.selected_track, Some(1));

        apply(
            &mut state,
            Message::Deck(DeckId(0), DeckMsg::SetTempo(80.0)),
        );
        let deck = state.decks.get(DeckId(0)).expect("deck A");
        assert_eq!(deck.view.timestretch.tempo, 50.0);

        apply(&mut state, Message::LoadOntoDeck(usize::MAX, DeckId(0)));
        let tracks = {
            let queue = state
                .decks
                .get(DeckId(0))
                .expect("deck A")
                .controller
                .queue();
            queue.append("https://example.test/pending.mp3").unwrap();
            queue.tracks()
        };
        let deck = state.decks.get_mut(DeckId(0)).expect("deck A");
        deck.ui.tracks = tracks;
        deck.ui.current_track_index = Some(0);
        apply(&mut state, Message::DeleteFocusedTrack);
        assert!(
            state
                .decks
                .get(DeckId(0))
                .expect("deck A")
                .controller
                .queue()
                .tracks()
                .is_empty()
        );

        state.ui.cache.set_layout(DeckLayout::Single);
        let hidden = state
            .decks
            .get(DeckId(1))
            .expect("deck B")
            .controller
            .clone();
        apply(&mut state, Message::PauseHiddenDecks);
        assert!(!hidden.queue().is_playing());

        apply(&mut state, Message::WindowResized(Size::new(640.0, 480.0)));
        assert!(state.ui.cache.window.caption().starts_with("640 × 480"));

        assert_eq!(
            update(
                &mut state,
                Message::Ui(UiEvent::Window(WindowCommand::Minimize)),
            )
            .units(),
            1
        );
        assert_eq!(update(&mut state, Message::Tick).units(), 0);
        assert_eq!(update(&mut state, Message::BroadcastToggle).units(), 0);
        assert_eq!(update(&mut state, Message::BroadcastToggle).units(), 0);
        assert_eq!(
            update(
                &mut state,
                Message::BroadcastStopped(Some(Duration::from_millis(10))),
            )
            .units(),
            0
        );
        assert_eq!(update(&mut state, Message::WindowCloseRequested).units(), 1);
    }

    #[kithara::test(native, flash(false))]
    fn eq_mode_changes_every_deck_as_one_transaction() {
        let mut state = test_fixture::state();
        let initial = [
            [-6.0f32, 2.0, 5.0].map(GainDb::from),
            [1.0f32, 3.0, 7.0].map(GainDb::from),
        ];
        for (deck, gains) in state.decks.iter().zip(initial) {
            deck.controller
                .mutate(|snapshot| snapshot.eq_bands = gains.to_vec());
        }

        apply(&mut state, Message::SetEqMode(EqMode::FourBand));
        assert_eq!(state.eq_mode, EqMode::FourBand);
        for (deck, gains) in state.decks.iter().zip(initial) {
            let expected = vec![gains[0], gains[1], gains[1], gains[2]];
            assert_eq!(deck.controller.snapshot().eq_bands, expected);
            assert_eq!(deck.controller.queue().eq_band_count(), 4);
        }

        apply(&mut state, Message::SetEqMode(EqMode::FourBand));
        apply(&mut state, Message::SetEqMode(EqMode::ThreeBand));
        assert_eq!(state.eq_mode, EqMode::ThreeBand);
        for (deck, gains) in state.decks.iter().zip(initial) {
            assert_eq!(deck.controller.snapshot().eq_bands, gains);
            assert_eq!(deck.controller.queue().eq_band_count(), 3);
        }
    }

    #[kithara::test(native, flash(false))]
    fn invalid_eq_snapshot_keeps_the_shared_mode_unchanged() {
        let mut state = test_fixture::state();
        state
            .decks
            .get(DeckId(0))
            .expect("deck A")
            .controller
            .mutate(|snapshot| snapshot.eq_bands.clear());

        apply(&mut state, Message::SetEqMode(EqMode::FourBand));

        assert_eq!(state.eq_mode, EqMode::ThreeBand);
        assert!(
            state
                .decks
                .iter()
                .all(|deck| deck.controller.queue().eq_band_count() == 3)
        );
    }
}
