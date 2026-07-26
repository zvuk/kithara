use iced::{
    Event as IcedEvent, Subscription, Task, Theme, event,
    event::Status,
    keyboard::{Event as KeyboardEvent, Key, key::Named},
    time as iced_time, window,
};
use kithara_platform::{sync::Arc, time::Duration};

use super::{
    deck::DeckUi, frontend::window_settings, message::Message, shot::ShotPlan, studio_ui::StudioUi,
    subscription::subscription_config, theme,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet},
    state::StateController,
    theme::gui,
};

/// Main GUI application state.
///
/// Each deck owns its shared model ([`crate::state::StateController`]) and its
/// own snapshot; the session mix is owned by [`DeckSet`]. This struct adds only
/// what belongs to no single deck: the highlighted catalog row and which deck
/// the keyboard talks to.
pub(crate) struct Kithara {
    pub(crate) session: DeckSet,
    pub(crate) decks: Decks,
    /// The app's track list; decks load from it.
    pub(crate) catalog: Catalog,
    /// Needed to build a track source when the catalog loads onto a deck.
    pub(crate) config: AppConfig,
    /// The compiled studio UI and its host-owned view state.
    pub(crate) studio: StudioUi,

    pub(crate) palette: gui::GuiPalette,
    /// Highlighted catalog row, shared by every deck's load buttons.
    pub(crate) selected_track: Option<usize>,
    /// The studio window; the screenshot pass captures it.
    pub(crate) window_id: window::Id,
    /// Present only under `KITHARA_SHOT_DIR`.
    pub(crate) shot: Option<ShotPlan>,
}

/// A non-empty set of deck view-models. `focus` is fixed to the first deck
/// at construction and backs the keyboard Delete shortcut only.
pub(crate) struct Decks {
    items: Vec<DeckUi>,
    focus: DeckId,
}

impl Decks {
    /// Returns `None` for an empty session — a GUI without a deck has nothing
    /// to render.
    pub(crate) fn new(controllers: Vec<(DeckId, Arc<StateController>)>) -> Option<Self> {
        let items: Vec<DeckUi> = controllers
            .into_iter()
            .map(|(id, controller)| DeckUi::new(id, controller))
            .collect();
        let focus = items.first()?.id;
        Some(Self { items, focus })
    }

    pub(crate) fn get(&self, id: DeckId) -> Option<&DeckUi> {
        self.items.iter().find(|deck| deck.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: DeckId) -> Option<&mut DeckUi> {
        self.items.iter_mut().find(|deck| deck.id == id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &DeckUi> {
        self.items.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut DeckUi> {
        self.items.iter_mut()
    }

    pub(crate) fn focus(&self) -> DeckId {
        self.focus
    }
}

impl Kithara {
    /// Boot function for `iced::daemon()`. Opens the studio window.
    pub(crate) fn new(
        session: DeckSet,
        decks: Decks,
        catalog: Catalog,
        config: AppConfig,
        palette: gui::GuiPalette,
    ) -> (Self, Task<Message>) {
        let (window_id, open) = window::open(window_settings());

        let state = Self {
            session,
            decks,
            catalog,
            config,
            studio: StudioUi::new(),
            palette,
            selected_track: None,
            window_id,
            shot: ShotPlan::read(),
        };

        (state, open.discard())
    }

    /// Time-tick subscription for player state sync plus keyboard. Tick
    /// interval scales with playback state to save CPU while idle.
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        const SUBSCRIPTION_CAPACITY: usize = 3;
        let playing = self.decks.iter().any(|deck| deck.ui.playing);
        let cfg = subscription_config(playing);
        let mut subs = Vec::with_capacity(SUBSCRIPTION_CAPACITY);
        subs.push(
            iced_time::every(Duration::from_millis(cfg.tick_interval_ms)).map(|_| Message::Tick),
        );
        subs.push(window::close_requests().map(|_| Message::WindowCloseRequested));
        if cfg.is_keyboard_enabled {
            subs.push(event::listen_with(|e, status, _window| match e {
                // Only act on Delete/Backspace the focused widget left
                // unhandled.
                IcedEvent::Keyboard(KeyboardEvent::KeyPressed {
                    key: Key::Named(Named::Delete | Named::Backspace),
                    ..
                }) if status == Status::Ignored => Some(Message::DeleteFocusedTrack),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    /// The dark + gold theme.
    pub(crate) fn theme(&self, _window: window::Id) -> Theme {
        theme::kithara_theme(&self.palette)
    }

    /// Window title.
    pub(crate) fn title(_state: &Self, _window: window::Id) -> String {
        "Kithara - DJ Studio".to_string()
    }
}
