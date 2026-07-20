use iced::{
    Color, Element, Length,
    widget::svg::{self, Handle as SvgHandle, Svg},
};

/// SVG icon available to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Icon {
    Disc,
    Collection,
    Folder,
    Faders,
    Gear,
    Home,
    Instrument,
    Lock,
    MusicNote,
    Pause,
    Play,
    PlaylistAdd,
    Playlist,
    Plus,
    RepeatOnce,
    Repeat,
    Shuffle,
    SkipBack,
    SkipForward,
    SpeakerHigh,
    SpeakerLow,
    SpeakerX,
    Search,
    Charts,
    Clock,
    Monitor,
    Usb,
    Waveform,
    Zvuk,
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
        Icon::Collection => include_bytes!("../../assets/icons/collection.svg"),
        Icon::Folder => include_bytes!("../../assets/icons/folder.svg"),
        Icon::Faders => include_bytes!("../../assets/icons/faders.svg"),
        Icon::Gear => include_bytes!("../../assets/icons/gear.svg"),
        Icon::Home => include_bytes!("../../assets/icons/home.svg"),
        Icon::Instrument => include_bytes!("../../assets/icons/instrument.svg"),
        Icon::Lock => include_bytes!("../../assets/icons/lock.svg"),
        Icon::MusicNote => include_bytes!("../../assets/icons/music-note.svg"),
        Icon::Pause => include_bytes!("../../assets/icons/pause.svg"),
        Icon::Play => include_bytes!("../../assets/icons/play.svg"),
        Icon::PlaylistAdd => include_bytes!("../../assets/icons/playlist-add.svg"),
        Icon::Playlist => include_bytes!("../../assets/icons/playlist.svg"),
        Icon::Plus => include_bytes!("../../assets/icons/plus.svg"),
        Icon::RepeatOnce => include_bytes!("../../assets/icons/repeat-once.svg"),
        Icon::Repeat => include_bytes!("../../assets/icons/repeat.svg"),
        Icon::Shuffle => include_bytes!("../../assets/icons/shuffle.svg"),
        Icon::SkipBack => include_bytes!("../../assets/icons/skip-back.svg"),
        Icon::SkipForward => include_bytes!("../../assets/icons/skip-forward.svg"),
        Icon::SpeakerHigh => include_bytes!("../../assets/icons/speaker-high.svg"),
        Icon::SpeakerLow => include_bytes!("../../assets/icons/speaker-low.svg"),
        Icon::SpeakerX => include_bytes!("../../assets/icons/speaker-x.svg"),
        Icon::Search => include_bytes!("../../assets/icons/search.svg"),
        Icon::Charts => include_bytes!("../../assets/icons/charts.svg"),
        Icon::Clock => include_bytes!("../../assets/icons/clock.svg"),
        Icon::Monitor => include_bytes!("../../assets/icons/monitor.svg"),
        Icon::Usb => include_bytes!("../../assets/icons/usb.svg"),
        Icon::Waveform => include_bytes!("../../assets/icons/waveform.svg"),
        Icon::Zvuk => include_bytes!("../../assets/icons/zvuk.svg"),
    }
}
