use std::{sync::Arc, time::Duration};

use iced::{
    Event as IcedEvent, Subscription, Task, Theme, event,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
    time as iced_time,
};

use super::{
    message::{Message, Tab},
    subscription::subscription_config,
    theme,
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
    pub(crate) ui_state: UiState,

    pub(crate) palette: gui::GuiPalette,
    pub(crate) active_tab: Tab,
    pub(crate) selected_track_index: Option<usize>,
    pub(crate) blink_counter: u8,
    pub(crate) url_text: String,
    pub(crate) previous_volume: f32,
}

impl Kithara {
    /// Boot function for `iced::application()`.
    pub(crate) fn new(
        controller: Arc<StateController>,
        palette: gui::GuiPalette,
    ) -> (Self, Task<Message>) {
        let ui_state = controller.snapshot();

        let state = Self {
            controller,
            previous_volume: ui_state.volume.max(0.01),
            ui_state,
            palette,
            active_tab: Tab::Playlist,
            selected_track_index: None,
            blink_counter: 0,
            url_text: String::new(),
        };

        (state, Task::none())
    }

    /// Time-tick subscription for player state sync plus keyboard. Tick
    /// interval scales with playback state to save CPU while idle.
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        const SUBSCRIPTION_CAPACITY: usize = 2;
        let cfg = subscription_config(self.ui_state.playing);
        let mut subs = Vec::with_capacity(SUBSCRIPTION_CAPACITY);
        subs.push(
            iced_time::every(Duration::from_millis(cfg.tick_interval_ms)).map(|_| Message::Tick),
        );
        if cfg.is_keyboard_enabled {
            subs.push(event::listen_with(|e, _status, _window| match e {
                IcedEvent::Keyboard(KeyboardEvent::KeyPressed {
                    key: Key::Named(Named::Delete | Named::Backspace),
                    ..
                }) => Some(Message::DeleteTrack),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self) -> Theme {
        theme::kithara_theme(&self.palette)
    }
}
