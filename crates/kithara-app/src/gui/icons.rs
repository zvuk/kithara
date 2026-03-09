use iced::{Color, Element, Length, widget::svg};

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
            Self::Play => include_bytes!("../../assets/icons/play.svg"),
            Self::Pause => include_bytes!("../../assets/icons/pause.svg"),
            Self::SkipNext => include_bytes!("../../assets/icons/skip-forward.svg"),
            Self::SkipPrev => include_bytes!("../../assets/icons/skip-back.svg"),
            Self::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
            Self::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
            Self::RepeatOnce => include_bytes!("../../assets/icons/repeat-once.svg"),
            Self::VolumeHigh => include_bytes!("../../assets/icons/speaker-high.svg"),
            Self::VolumeLow => include_bytes!("../../assets/icons/speaker-low.svg"),
            Self::VolumeMute => include_bytes!("../../assets/icons/speaker-x.svg"),
            Self::Playlist => include_bytes!("../../assets/icons/playlist.svg"),
            Self::Equalizer => include_bytes!("../../assets/icons/faders.svg"),
            Self::Settings => include_bytes!("../../assets/icons/gear.svg"),
            Self::Dj => include_bytes!("../../assets/icons/disc.svg"),
            Self::MusicNote => include_bytes!("../../assets/icons/music-note.svg"),
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
}
