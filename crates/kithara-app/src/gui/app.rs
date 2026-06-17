use std::sync::Arc;

use iced::{
    Event as IcedEvent, Subscription, Task, Theme, event,
    event::Status,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
    time as iced_time, window,
};
use kithara_platform::time::Duration;

use super::{
    dj::DjView,
    frontend::window_settings,
    message::{Message, Tab},
    subscription::subscription_config,
    theme,
    url_bar::UrlBar,
};
use crate::{
    state::{StateController, UiState},
    theme::gui,
};

/// Main GUI application state.
///
/// Player state lives in [`StateController`]; this struct only holds
/// view-local state that has no business in the shared model
/// (selected row, active tab, transient text input, blink counter).
/// `ui_state` is refreshed once per `Tick` so all view code reads from
/// a single, consistent snapshot.
pub(crate) struct Kithara {
    pub(crate) controller: Arc<StateController>,
    /// View-local DJ Studio state (open / closed).
    pub(crate) dj: DjView,

    pub(crate) palette: gui::GuiPalette,
    pub(crate) selected_track_index: Option<usize>,
    /// Currently live window. One window is open at a time; the DJ-mode
    /// swap opens the new window and closes this one.
    pub(crate) window_id: Option<window::Id>,
    pub(crate) active_tab: Tab,
    pub(crate) ui_state: UiState,
    pub(crate) url: UrlBar,
    pub(crate) previous_volume: f32,
    pub(crate) blink_counter: u8,
}

impl Kithara {
    /// Boot function for `iced::daemon()`. Opens the initial compact
    /// window and tracks its id.
    pub(crate) fn new(
        controller: Arc<StateController>,
        palette: gui::GuiPalette,
    ) -> (Self, Task<Message>) {
        let ui_state = controller.snapshot();

        let mut state = Self {
            controller,
            previous_volume: ui_state.volume.max(0.01),
            ui_state,
            palette,
            active_tab: Tab::Playlist,
            selected_track_index: None,
            blink_counter: 0,
            url: UrlBar::default(),
            dj: DjView::default(),
            window_id: None,
        };

        let (id, open) = window::open(window_settings(state.dj.open));
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
                // Only act on Delete/Backspace the focused widget left
                // unhandled. A focused text input (URL bar) captures these
                // for editing, so the playlist shortcut must not also fire.
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

    /// Window title, reflecting the active mode.
    pub(crate) fn title(&self, _window: window::Id) -> String {
        if self.dj.open {
            "Kithara - DJ Studio".to_string()
        } else {
            "Kithara".to_string()
        }
    }
}
