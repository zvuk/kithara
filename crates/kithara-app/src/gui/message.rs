use kithara_ui::render::UiEvent;

use crate::deck::DeckId;

/// All GUI events flow through this enum.
///
/// Deck-scoped events carry the deck they address; blocks themselves emit
/// [`super::deck::DeckMsg`] and know nothing about deck identity.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Raw event from the compiled studio UI; translated by
    /// [`super::studio_ui::translate`].
    Ui(UiEvent),
    /// Event addressed to one deck.
    Deck(DeckId, super::deck::DeckMsg),
    /// Session-mix edit (crossfader, trim, mute, master).
    Mix(super::mix::MixMsg),
    /// Delete the current track of the focused deck (keyboard shortcut;
    /// the subscription has no access to the focus).
    DeleteFocusedTrack,
    /// Highlight a catalog row.
    SelectCatalogTrack(usize),
    /// Load catalog row `.0` onto deck `.1`.
    LoadOntoDeck(usize, DeckId),
    /// Remove catalog row `.0` from deck `.1`'s queue.
    UnloadFromDeck(usize, DeckId),
    /// Periodic tick from the subscription.
    Tick,
    /// System close button on the studio window; exits the app.
    WindowCloseRequested,
}
