use iced::{Subscription, Task, Theme, event, time as iced_time, window};
use kithara_platform::{sync::Arc, time::Duration};

use super::{
    deck::DeckUi,
    frontend::window_settings,
    message::Message,
    studio_ui::{StudioUi, deletes_focused_track},
    subscription::subscription_config,
    theme,
};
use crate::{
    catalog::Catalog,
    config::AppConfig,
    deck::{DeckId, DeckSet, EqMode},
    state::StateController,
    theme::gui,
};

/// Main GUI application state.
///
/// Each deck owns its shared model ([`crate::state::StateController`]) and its
/// own snapshot; the session mix is owned by [`DeckSet`]. This struct adds only
/// what belongs to no single deck: the highlighted catalog row and the studio
/// window.
pub(crate) struct Kithara {
    /// Needed to build a track source when the catalog loads onto a deck.
    pub(crate) config: AppConfig,
    /// The app's track list; decks load from it.
    pub(crate) catalog: Catalog,
    pub(crate) session: DeckSet,
    pub(crate) decks: Decks,
    /// One EQ topology shared by every deck in the studio.
    pub(crate) eq_mode: EqMode,
    pub(crate) palette: gui::GuiPalette,

    /// The studio window; window-chrome commands execute against it.
    pub(crate) window_id: window::Id,
    /// Highlighted catalog row, shared by every deck's load buttons.
    pub(crate) selected_track: Option<usize>,
    /// The compiled studio UI and its host-owned view state.
    pub(crate) studio: StudioUi,
}

/// A non-empty set of deck view-models, addressed by id.
pub(crate) struct Decks {
    items: Vec<DeckUi>,
}

impl Decks {
    /// Returns `None` for an empty session — a GUI without a deck has nothing
    /// to render.
    pub(crate) fn new(controllers: Vec<(DeckId, Arc<StateController>)>) -> Option<Self> {
        let items: Vec<DeckUi> = controllers
            .into_iter()
            .map(|(id, controller)| DeckUi::new(id, controller))
            .collect();
        (!items.is_empty()).then_some(Self { items })
    }

    pub(crate) fn get(&self, id: DeckId) -> Option<&DeckUi> {
        self.items.iter().find(|deck| deck.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: DeckId) -> Option<&mut DeckUi> {
        self.items.iter_mut().find(|deck| deck.id == id)
    }

    delegate::delegate! {
        to self.items {
            pub(crate) fn iter(&self) -> impl Iterator<Item = &DeckUi>;
            pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut DeckUi>;
        }
    }
}

impl Kithara {
    /// Boot function for `iced::daemon()`. Opens the studio window.
    pub(crate) fn new(
        session: DeckSet,
        decks: Decks,
        catalog: Catalog,
        config: AppConfig,
        studio: StudioUi,
        palette: gui::GuiPalette,
    ) -> (Self, Task<Message>) {
        let (window_id, open) = window::open(window_settings());

        (
            Self::mounted(session, decks, catalog, config, studio, palette, window_id),
            open.discard(),
        )
    }

    /// The same state without a window of iced's: a host that owns its own
    /// window mounts the studio through here.
    pub(crate) fn mounted(
        session: DeckSet,
        decks: Decks,
        catalog: Catalog,
        config: AppConfig,
        studio: StudioUi,
        palette: gui::GuiPalette,
        window_id: window::Id,
    ) -> Self {
        Self {
            session,
            decks,
            catalog,
            config,
            studio,
            eq_mode: EqMode::default(),
            palette,
            window_id,
            selected_track: None,
        }
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
            subs.push(event::listen_with(|event, status, _window| {
                deletes_focused_track(&event, status).then_some(Message::DeleteFocusedTrack)
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
