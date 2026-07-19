use crate::deck::DeckId;

/// All GUI events flow through this enum.
///
/// Deck-scoped events carry the deck they address; blocks themselves emit
/// [`super::deck::DeckMsg`] and know nothing about deck identity.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Event addressed to one deck.
    Deck(DeckId, super::deck::DeckMsg),
    /// Session-mix edit (crossfader, trim, mute, master).
    Mix(super::mix::MixMsg),
    /// URL bar control event (grouped to keep this enum thin).
    Url(super::url_bar::UrlMsg),
    /// Switch the active tab.
    TabSelected(Tab),
    /// Delete the highlighted track of the focused deck (keyboard shortcut;
    /// the subscription has no access to the focus).
    DeleteFocusedTrack,
    /// Highlight a catalog row (a second click loads it onto the focused deck).
    SelectCatalogTrack(usize),
    /// Load catalog row `.0` onto deck `.1`.
    LoadOntoDeck(usize, DeckId),
    /// Move the keyboard / library focus to a deck.
    FocusDeck(DeckId),
    /// Swap the live window between DJ Studio and the compact layout.
    ToggleStudio,
    /// Periodic tick from the subscription (100 ms).
    Tick,
    /// System close button on a window. Exits the app only for the live
    /// window; the mode-swap window is closed programmatically.
    WindowCloseRequested(iced::window::Id),
}

/// Tabs in the main content area. Mirrors the iOS reference layout
/// (Playlist / EQ / Settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Playlist,
    Equalizer,
    Settings,
}
