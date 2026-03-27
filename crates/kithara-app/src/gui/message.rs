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
    SeekChanged(f32),
    /// Seek slider released — commit the seek.
    SeekReleased,
    /// Volume slider changed (0.0 – 1.0).
    VolumeChanged(f32),
    /// EQ band gain changed (band index, dB value).
    EqBandChanged(usize, f32),
    /// Reset a single EQ band to 0 dB.
    EqBandReset(usize),
    /// Playback rate changed.
    PlayRateChanged(f32),
    /// Crossfade duration changed (seconds).
    CrossfadeChanged(f32),
    /// Select a track from the playlist by index.
    SelectTrack(usize),
    /// Switch the active tab.
    TabSelected(Tab),
    /// A track finished loading asynchronously (index, result).
    TrackLoaded(usize, Result<(), String>),
    /// Switch ABR mode (None = Auto).
    SetAbrMode(Option<usize>),
    /// Periodic tick from the subscription (100 ms).
    Tick,
    /// Toggle shuffle mode.
    ToggleShuffle,
    /// Toggle repeat mode.
    ToggleRepeat,
}

/// Tabs in the main content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Playlist,
    Dj,
    Equalizer,
    Settings,
}
