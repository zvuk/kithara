use iced::{Color, Element, Length, widget::svg};

/// Gold accent from the kithara logo (#bb9442).
pub(crate) const GOLD: Color = Color {
    r: 0.733,
    g: 0.580,
    b: 0.259,
    a: 1.0,
};

/// Muted gray for inactive elements (#888 for contrast on dark bg).
pub(crate) const MUTED: Color = Color {
    r: 0.533,
    g: 0.533,
    b: 0.533,
    a: 1.0,
};

/// Light text / neutral icon color.
pub(crate) const LIGHT: Color = Color {
    r: 0.900,
    g: 0.900,
    b: 0.900,
    a: 1.0,
};

/// Phosphor Regular SVG icons used throughout the UI.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Icon {
    Play,
    Pause,
    SkipNext,
    SkipPrev,
    Shuffle,
    Repeat,
    RepeatOnce,
    VolumeHigh,
    VolumeLow,
    VolumeMute,
    Playlist,
    Equalizer,
    Settings,
    Dj,
    MusicNote,
}

impl Icon {
    /// Raw SVG bytes embedded at compile time.
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Play => include_bytes!("../assets/icons/play.svg"),
            Self::Pause => include_bytes!("../assets/icons/pause.svg"),
            Self::SkipNext => include_bytes!("../assets/icons/skip-forward.svg"),
            Self::SkipPrev => include_bytes!("../assets/icons/skip-back.svg"),
            Self::Shuffle => include_bytes!("../assets/icons/shuffle.svg"),
            Self::Repeat => include_bytes!("../assets/icons/repeat.svg"),
            Self::RepeatOnce => include_bytes!("../assets/icons/repeat-once.svg"),
            Self::VolumeHigh => include_bytes!("../assets/icons/speaker-high.svg"),
            Self::VolumeLow => include_bytes!("../assets/icons/speaker-low.svg"),
            Self::VolumeMute => include_bytes!("../assets/icons/speaker-x.svg"),
            Self::Playlist => include_bytes!("../assets/icons/playlist.svg"),
            Self::Equalizer => include_bytes!("../assets/icons/faders.svg"),
            Self::Settings => include_bytes!("../assets/icons/gear.svg"),
            Self::Dj => include_bytes!("../assets/icons/disc.svg"),
            Self::MusicNote => include_bytes!("../assets/icons/music-note.svg"),
        }
    }

    /// Render this icon as an SVG widget with the given size and color.
    pub(crate) fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        let handle = svg::Handle::from_memory(self.bytes());
        svg::Svg::new(handle)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| svg::Style { color: Some(color) })
            .into()
    }

    /// Render in gold (active / accent).
    pub(crate) fn gold<'a, M: 'a>(self, size: f32) -> Element<'a, M> {
        self.view(size, GOLD)
    }

    /// Render in muted gray (inactive).
    #[expect(dead_code, reason = "useful helper, not yet used in current UI")]
    pub(crate) fn muted<'a, M: 'a>(self, size: f32) -> Element<'a, M> {
        self.view(size, MUTED)
    }

    /// Render in light color (neutral).
    pub(crate) fn light<'a, M: 'a>(self, size: f32) -> Element<'a, M> {
        self.view(size, LIGHT)
    }
}
