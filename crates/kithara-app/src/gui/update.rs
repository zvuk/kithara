use iced::{
    Task, window,
    window::{Direction, Mode},
};
use kithara::{
    host::DeckId,
    platform::time::Duration,
    ui::render::{WindowCommand, WindowEdge},
};
use tracing::warn;

use super::{
    app::Kithara,
    deck::{self, DeckMsg},
    message::Message,
    subscription::subscription_config,
    ui,
};
use crate::engine::{AppCmd, Command, DeckCmd};

pub(crate) fn update(state: &mut Kithara, message: Message) -> Task<Message> {
    let task = match message {
        #[cfg(not(target_arch = "wasm32"))]
        Message::AddMusicFolder => {
            state.picker.open();
            Task::none()
        }
        Message::BroadcastToggle => {
            state.send(Command::App(AppCmd::BroadcastToggle));
            Task::none()
        }
        Message::Ui(published) => {
            if let Some(translated) =
                ui::settle(state, published).and_then(|event| ui::translate(state, event))
            {
                return update(state, translated);
            }
            Task::none()
        }
        Message::Deck(id, msg) => {
            handle_deck(state, id, &msg);
            Task::none()
        }
        Message::SetTempo(tempo) => {
            state.send(Command::App(AppCmd::SetTempo(tempo)));
            Task::none()
        }
        Message::StepTempo(steps) => {
            state.send(Command::App(AppCmd::StepTempo(steps)));
            Task::none()
        }
        Message::SetEqMode(mode) => {
            state.send(Command::App(AppCmd::SetEqMode(mode)));
            Task::none()
        }
        Message::Mix(cmd) => {
            state.send(Command::Mix(cmd));
            Task::none()
        }
        Message::DeleteFocusedTrack => {
            delete_focused_track(state);
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
        Message::WindowCloseRequested => close(state),
    };

    state.refresh();
    task
}

fn close(state: &mut Kithara) -> Task<Message> {
    state.send(Command::App(AppCmd::Shutdown));
    iced::exit()
}

/// The app draws its own window chrome, so the app executes what the bar
/// asks against the window it opened.
fn window_task(state: &mut Kithara, command: WindowCommand) -> Task<Message> {
    match command {
        WindowCommand::Drag => window::drag(state.window_id),
        WindowCommand::Resize(edge) => direction(edge).map_or_else(Task::none, |direction| {
            window::drag_resize(state.window_id, direction)
        }),
        WindowCommand::Minimize => window::minimize(state.window_id, true),
        WindowCommand::ToggleMaximize => window::toggle_maximize(state.window_id),
        WindowCommand::ToggleFullScreen => toggle_full_screen(state.window_id),
        WindowCommand::Close => close(state),
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
    let shown = state.ui.cache.layout().letters();

    let hidden: Vec<DeckId> = state
        .snapshot
        .decks
        .iter()
        .filter(|deck| !shown.contains(&deck.letter))
        .map(|deck| deck.id)
        .collect();

    for id in hidden {
        state.send(Command::Deck {
            cmd: DeckCmd::Pause,
            deck: id,
        });
    }
}

fn delete_focused_track(state: &mut Kithara) {
    let focus = state.ui.cache.focus_deck();
    let Some(id) = state.snapshot.lettered(focus).map(|deck| deck.id) else {
        return;
    };

    handle_deck(state, id, &DeckMsg::DeleteTrack);
}

fn handle_deck(state: &mut Kithara, id: DeckId, msg: &DeckMsg) {
    let Some(cmd) = state
        .snapshot
        .deck(id)
        .and_then(|shown| deck::command(shown, state.snapshot.eq_mode, msg))
    else {
        return;
    };
    state.send(Command::Deck { cmd, deck: id });
}

fn handle_tick(state: &mut Kithara) {
    state.library.tick();
    let playing = state.snapshot.decks.iter().any(|deck| deck.playing);
    state.ui.advance(Duration::from_millis(
        subscription_config(playing).tick_interval_ms,
    ));
}

#[cfg(all(test, not(feature = "broadcast")))]
mod tests {
    use std::{convert::Infallible, mem};

    use ::kithara::{
        effects::GainDb,
        ui::render::{ControlAction, Published, WindowCommand, WindowEdge},
    };
    use iced::{Size, window::Direction};
    use kithara_test_utils::{kithara, off_thread::OffThread};

    use super::*;
    use crate::{
        analysis::fixtures::long_wav,
        deck::EqMode,
        engine::Envelope,
        gui::{rig::Rig, ui::cache::DeckLayout},
    };

    fn apply(rig: &mut Rig, message: Message) {
        rig.message(message);
        rig.pump();
        rig.ui.refresh();
    }

    fn apply_eq(rig: &mut Rig, message: Message) {
        apply(rig, message);
        crate::mock::render_rig(rig, 128);
        rig.engine.publish();
        rig.ui.refresh();
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
        let mut rig = Rig::offline();
        let commands = [
            WindowCommand::Drag,
            WindowCommand::Resize(WindowEdge::North),
            WindowCommand::Minimize,
            WindowCommand::ToggleMaximize,
            WindowCommand::ToggleFullScreen,
            WindowCommand::Close,
        ];

        for command in commands {
            assert_eq!(window_task(&mut rig.ui, command).units(), 1, "{command:?}");
        }
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn update_routes_messages_and_refreshes_their_state() {
        let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");

        rig.call(|rig| {
            apply(
                rig,
                Message::Ui(Published::Gesture {
                    action: ControlAction::SetScalar(1.0),
                    path: "mixer/xfade".to_string(),
                }),
            );
            assert_eq!(rig.snapshots.load().mix.position, 1.0);
            assert_eq!(rig.ui.snapshot.mix.position, 1.0);

            let (a, b) = (rig.deck("a"), rig.deck("b"));
            apply(rig, Message::Deck(a, DeckMsg::SetTempo(80.0.into())));
            let deck = rig.ui.snapshot.deck(a).expect("deck A");
            assert_eq!(f32::from(deck.tempo), 50.0);

            let queue = rig.queues[0].clone();
            queue.append("https://example.test/pending.mp3").unwrap();
            rig.shows("a", |deck| {
                deck.tracks = queue.tracks();
                deck.current_track_index = Some(0);
            });

            apply(rig, Message::DeleteFocusedTrack);
            assert!(queue.tracks().is_empty());

            rig.ui.ui.cache.set_layout(DeckLayout::Single);
            rig.message(Message::PauseHiddenDecks);

            let queued: Vec<Envelope> =
                std::iter::from_fn(|| rig.commands.try_recv().ok()).collect();
            assert!(
                matches!(
                    queued.as_slice(),
                    [Envelope {
                        command: Command::Deck {
                            deck,
                            cmd: DeckCmd::Pause,
                        },
                        ..
                    }] if *deck == b
                ),
                "only the hidden deck is paused: {queued:?}"
            );

            apply(rig, Message::WindowResized(Size::new(640.0, 480.0)));
            assert!(rig.ui.ui.cache.window.caption().starts_with("640 × 480"));

            assert_eq!(
                update(
                    &mut rig.ui,
                    Message::Ui(Published::window(WindowCommand::Minimize))
                )
                .units(),
                1
            );
            assert_eq!(update(&mut rig.ui, Message::Tick).units(), 0);

            apply(rig, Message::BroadcastToggle);
            apply(rig, Message::BroadcastToggle);

            assert_eq!(
                update(&mut rig.ui, Message::WindowCloseRequested).units(),
                1
            );
        })
        .await;

        rig.close().await;
    }

    #[kithara::test(native, tokio, flash(false))]
    async fn a_play_press_follows_the_queue(long_wav: String) {
        let rig = OffThread::spawn("app-host", || Ok::<_, Infallible>(Rig::offline()))
            .await
            .expect("rig fixture is infallible");

        rig.call(move |rig| {
            let queue = rig.queues[0].clone();
            queue
                .append(long_wav.as_str())
                .expect("deck A takes the track");
            queue.play();

            rig.until(
                "the queue plays",
                Rig::DEADLINE,
                |_| {},
                |_| queue.is_playing(),
            );

            rig.shows("a", |deck| deck.playing = false);
            assert!(!rig.flag("deck.playback.playing@deck=a"));

            rig.send("deck-a/play", ControlAction::Activate);
            assert!(
                !rig.flag("deck.playback.playing@deck=a"),
                "the play control draws the published playback state"
            );

            rig.pump();
            rig.until(
                "the press pauses the playing queue",
                Rig::DEADLINE,
                |_| {},
                |_| !queue.is_playing(),
            );

            rig.shows("a", |deck| deck.playing = true);
            assert!(rig.flag("deck.playback.playing@deck=a"));

            rig.send("deck-a/play", ControlAction::Activate);
            rig.pump();
            rig.until(
                "the press plays the paused queue",
                Rig::DEADLINE,
                |_| {},
                |_| queue.is_playing(),
            );
        })
        .await;

        rig.close().await;
    }

    #[kithara::test(native, flash(false))]
    #[kithara::hang_watchdog]
    fn eq_mode_changes_every_deck_as_one_transaction() {
        let mut rig = Rig::offline();

        let initial = [
            [-6.0f32, 2.0, 5.0].map(GainDb::from),
            [1.0f32, 3.0, 7.0].map(GainDb::from),
        ];
        for (id, gains) in [rig.deck("a"), rig.deck("b")].into_iter().zip(initial) {
            hang_tick!();
            for (band, gain) in gains.into_iter().enumerate() {
                hang_tick!();
                hang_park!(|_| {
                    apply_eq(
                        &mut rig,
                        Message::Deck(id, DeckMsg::EqBandChanged(band, gain)),
                    );
                });
                hang_reset!();
            }
            hang_reset!();
        }

        apply_eq(&mut rig, Message::SetEqMode(EqMode::FourBand));
        assert_eq!(rig.ui.snapshot.eq_mode, EqMode::FourBand);

        for ((deck, queue), gains) in rig.ui.snapshot.decks.iter().zip(&rig.queues).zip(initial) {
            let expected = vec![gains[0], gains[1], gains[1], gains[2]];
            assert_eq!(deck.eq_bands, expected);
            assert_eq!(queue.eq_band_count(), 4);
        }

        apply_eq(&mut rig, Message::SetEqMode(EqMode::FourBand));
        apply_eq(&mut rig, Message::SetEqMode(EqMode::ThreeBand));
        assert_eq!(rig.ui.snapshot.eq_mode, EqMode::ThreeBand);

        for ((deck, queue), gains) in rig.ui.snapshot.decks.iter().zip(&rig.queues).zip(initial) {
            assert_eq!(deck.eq_bands, gains);
            assert_eq!(queue.eq_band_count(), 3);
        }
    }

    #[kithara::test(native, flash(false))]
    fn invalid_eq_snapshot_keeps_the_shared_mode_unchanged() {
        let mut rig = Rig::offline_with(|config| config.eq_bands = 2);

        apply(&mut rig, Message::SetEqMode(EqMode::FourBand));

        assert_eq!(rig.ui.snapshot.eq_mode, EqMode::ThreeBand);
        assert!(rig.queues.iter().all(|queue| queue.eq_band_count() == 2));
    }
}
