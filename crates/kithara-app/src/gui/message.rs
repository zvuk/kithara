use kithara::{
    host::{DeckId, api::Tempo},
    ui::render::{Published, WindowCommand},
};

use crate::{deck::EqMode, engine::MixCmd};

/// All GUI events flow through this enum.
///
/// Deck-scoped events carry the deck they address; blocks themselves emit
/// [`super::deck::DeckMsg`] and know nothing about deck identity.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Ask the user for a folder to add to the library's Music Folders.
    #[cfg(not(target_arch = "wasm32"))]
    AddMusicFolder,
    BroadcastToggle,
    /// Move the host tempo.
    SetTempo(Tempo),
    StepTempo(f32),
    /// What the compiled UI published; settled against the document's
    /// writes, then translated by [`super::ui::translate`].
    Ui(Published),
    /// Event addressed to one deck.
    Deck(DeckId, super::deck::DeckMsg),
    /// Replace the EQ topology of every deck.
    SetEqMode(EqMode),
    /// Session-mix edit (crossfader, trim).
    Mix(MixCmd),
    /// Delete the current track of the focused deck (keyboard shortcut;
    /// the subscription has no access to the focus).
    DeleteFocusedTrack,
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
