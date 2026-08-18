use kithara_platform::time::Duration;
use kithara_ui::render::{UiEvent, WindowCommand};

use crate::deck::{DeckId, EqMode};

/// All GUI events flow through this enum.
///
/// Deck-scoped events carry the deck they address; blocks themselves emit
/// [`super::deck::DeckMsg`] and know nothing about deck identity.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    BroadcastToggle,
    BroadcastStopped(Option<Duration>),
    /// Raw event from the compiled UI; translated by
    /// [`super::ui::translate`].
    Ui(UiEvent),
    /// Event addressed to one deck.
    Deck(DeckId, super::deck::DeckMsg),
    /// Replace the EQ topology of every deck.
    SetEqMode(EqMode),
    /// Session-mix edit (crossfader, trim).
    Mix(super::mix::MixMsg),
    /// Delete the current track of the focused deck (keyboard shortcut;
    /// the subscription has no access to the focus).
    DeleteFocusedTrack,
    /// Highlight a catalog row.
    SelectCatalogTrack(usize),
    /// Load catalog row `.0` onto deck `.1`.
    LoadOntoDeck(usize, DeckId),
    /// Pause every deck the current layout does not lay out.
    PauseHiddenDecks,
    /// Periodic tick from the subscription.
    Tick,
    /// Window chrome the bar draws itself; executed against the
    /// window this app owns.
    Window(WindowCommand),
    /// The window settled at a new size; the menu draws it.
    WindowResized(iced::Size),
    /// The window manager asked the window to close; exits the app.
    WindowCloseRequested,
}
