/// All GUI events flow through this enum.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    /// Toggle play / pause.
    TogglePlayPause,
    /// Skip to next track.
    Next,
    /// Skip to previous track.
    Prev,
    /// Seek slider moved (position in seconds).
    SeekChanged(f64),
    /// Seek slider released — commit the seek.
    SeekReleased,
    /// Volume slider changed (0.0 – 1.0).
    VolumeChanged(f32),
    /// EQ band gain changed (band index, dB value).
    EqBandChanged(usize, f32),
    /// Playback rate changed.
    PlayRateChanged(f32),
    /// Crossfade duration changed (seconds).
    CrossfadeChanged(f32),
    /// URL text field changed.
    UrlChanged(String),
    /// Add URL from text field to queue.
    AddUrl,
    /// Toggle mute state.
    ToggleMute,
    /// Toggle shuffle on / off.
    ToggleShuffle,
    /// Cycle repeat mode (Off -> All -> One -> Off).
    ToggleRepeat,
    /// Reset all EQ bands to 0 dB.
    EqResetAll,
    /// Select a track from the playlist by index. First click just
    /// highlights the row (no playback); a second click on the already-
    /// selected row plays it. Matches common file-browser UX.
    SelectTrack(usize),
    /// Delete the currently-highlighted track (or current one if none
    /// is highlighted). Wired to the Delete / Backspace key.
    DeleteTrack,
    /// Switch the active tab.
    TabSelected(Tab),
    /// Switch ABR mode (None = Auto).
    SetAbrMode(Option<usize>),
    /// Periodic tick from the subscription (100 ms).
    Tick,
    /// DJ Studio control event (grouped to keep this enum thin).
    Dj(super::dj::DjMsg),
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
