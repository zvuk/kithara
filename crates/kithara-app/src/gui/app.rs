use iced::{
    Event as IcedEvent, Subscription, Task, Theme, event,
    event::Status,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
    time as iced_time, window,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_ui::render::RenderPalette;

use super::{
    frontend::window_settings,
    message::Message,
    modular::{ModularView, initial_view},
    subscription::subscription_config,
    theme,
};
use crate::{
    config::WindowSizing,
    state::{StateController, UiState},
};

/// Main GUI application state.
///
/// Player state lives in [`StateController`]; this struct only holds
/// view-local state that has no business in the shared model.
/// `ui_state` is refreshed once per `Tick` so all view code reads from
/// a single, consistent snapshot.
pub(crate) struct Kithara {
    pub(crate) controller: Arc<StateController>,
    pub(crate) modular: ModularView,
    pub(crate) settings_window_id: Option<window::Id>,

    pub(crate) library_query: String,
    pub(crate) palette: RenderPalette,
    pub(crate) selected_track_index: Option<usize>,
    pub(crate) window_sizing: WindowSizing,
    /// Currently live main window; mode swaps replace this ID while the
    /// optional settings window is tracked separately.
    pub(crate) window_id: Option<window::Id>,
    pub(crate) ui_state: UiState,
}

impl Kithara {
    /// Boot function for `iced::daemon()`. Opens the modular player window.
    pub(crate) fn new(
        controller: Arc<StateController>,
        palette: RenderPalette,
        window_sizing: WindowSizing,
    ) -> (Self, Task<Message>) {
        let ui_state = controller.snapshot();

        let mut state = Self {
            controller,
            ui_state,
            library_query: String::new(),
            palette,
            selected_track_index: None,
            window_sizing,
            modular: initial_view(),
            settings_window_id: None,
            window_id: None,
        };

        let (id, open) = window::open(window_settings(
            state.modular.compiled.as_ref(),
            &state.window_sizing,
        ));
        state.window_id = Some(id);

        (state, open.discard())
    }

    /// Time-tick subscription for player state sync plus keyboard. Tick
    /// interval scales with playback state to save CPU while idle.
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        const SUBSCRIPTION_CAPACITY: usize = 3;
        let cfg = subscription_config(self.ui_state.playing);
        let mut subs = Vec::with_capacity(SUBSCRIPTION_CAPACITY);
        subs.push(
            iced_time::every(Duration::from_millis(cfg.tick_interval_ms)).map(|_| Message::Tick),
        );
        subs.push(window::close_requests().map(Message::WindowCloseRequested));
        if cfg.is_keyboard_enabled {
            subs.push(event::listen_with(|e, status, _window| match e {
                // Only act on Delete/Backspace when the focused widget left
                // the key unhandled.
                IcedEvent::Keyboard(KeyboardEvent::KeyPressed {
                    key: Key::Named(Named::Delete | Named::Backspace),
                    ..
                }) if status == Status::Ignored => Some(Message::DeleteTrack),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self, _window: window::Id) -> Theme {
        theme::kithara_theme(&self.palette)
    }

    /// Static application title for every window.
    pub(crate) fn title(_state: &Self, _window: window::Id) -> String {
        "Kithara".to_owned()
    }
}
