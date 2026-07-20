use iced::{
    Color, Element, Length,
    widget::svg::{self, Handle as SvgHandle, Svg},
};

/// Phosphor Regular SVG icons used throughout the UI.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Icon {
    Play,
    Pause,
    SkipNext,
    SkipPrev,
    VolumeHigh,
    VolumeLow,
    VolumeMute,
    Playlist,
    Equalizer,
    Settings,
    Shuffle,
    Repeat,
    RepeatOnce,
    Disc,
    PlaylistAdd,
    /// Key-lock pill of the timestretch panel.
    #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
    Lock,
}

fn icon_bytes(icon: Icon) -> &'static [u8] {
    match icon {
        Icon::Play => include_bytes!("../../assets/icons/play.svg"),
        Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        Icon::SkipNext => include_bytes!("../../assets/icons/skip-forward.svg"),
        Icon::SkipPrev => include_bytes!("../../assets/icons/skip-back.svg"),
        Icon::VolumeHigh => include_bytes!("../../assets/icons/speaker-high.svg"),
        Icon::VolumeLow => include_bytes!("../../assets/icons/speaker-low.svg"),
        Icon::VolumeMute => include_bytes!("../../assets/icons/speaker-x.svg"),
        Icon::Playlist => include_bytes!("../../assets/icons/playlist.svg"),
        Icon::Equalizer => include_bytes!("../../assets/icons/faders.svg"),
        Icon::Settings => include_bytes!("../../assets/icons/gear.svg"),
        Icon::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
        Icon::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
        Icon::RepeatOnce => include_bytes!("../../assets/icons/repeat-once.svg"),
        Icon::Disc => include_bytes!("../../assets/icons/disc.svg"),
        Icon::PlaylistAdd => include_bytes!("../../assets/icons/playlist-add.svg"),
        #[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
        Icon::Lock => include_bytes!("../../assets/icons/lock.svg"),
    }
}

impl Icon {
    /// Render this icon as an SVG widget with the given size and color.
    pub(crate) fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        let handle = SvgHandle::from_memory(icon_bytes(self));
        Svg::new(handle)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| svg::Style { color: Some(color) })
            .into()
    }
}
