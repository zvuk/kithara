use iced::{
    Color, Element, Length,
    widget::svg::{self, Handle as SvgHandle, Svg},
};

/// SVG icon available to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Icon {
    Disc,
    Faders,
    Gear,
    Lock,
    MusicNote,
    Pause,
    Play,
    PlaylistAdd,
    Playlist,
    RepeatOnce,
    Repeat,
    Shuffle,
    SkipBack,
    SkipForward,
    SpeakerHigh,
    SpeakerLow,
    SpeakerX,
}

impl Icon {
    /// Renders this icon as an SVG widget with the given size and color.
    #[must_use]
    pub fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        let handle = SvgHandle::from_memory(icon_bytes(self));
        Svg::new(handle)
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .style(move |_theme, _status| svg::Style { color: Some(color) })
            .into()
    }
}

fn icon_bytes(icon: Icon) -> &'static [u8] {
    match icon {
        Icon::Disc => include_bytes!("../../assets/icons/disc.svg"),
        Icon::Faders => include_bytes!("../../assets/icons/faders.svg"),
        Icon::Gear => include_bytes!("../../assets/icons/gear.svg"),
        Icon::Lock => include_bytes!("../../assets/icons/lock.svg"),
        Icon::MusicNote => include_bytes!("../../assets/icons/music-note.svg"),
        Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        Icon::Play => include_bytes!("../../assets/icons/play.svg"),
        Icon::PlaylistAdd => include_bytes!("../../assets/icons/playlist-add.svg"),
        Icon::Playlist => include_bytes!("../../assets/icons/playlist.svg"),
        Icon::RepeatOnce => include_bytes!("../../assets/icons/repeat-once.svg"),
        Icon::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
        Icon::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
        Icon::SkipBack => include_bytes!("../../assets/icons/skip-back.svg"),
        Icon::SkipForward => include_bytes!("../../assets/icons/skip-forward.svg"),
        Icon::SpeakerHigh => include_bytes!("../../assets/icons/speaker-high.svg"),
        Icon::SpeakerLow => include_bytes!("../../assets/icons/speaker-low.svg"),
        Icon::SpeakerX => include_bytes!("../../assets/icons/speaker-x.svg"),
    }
}
