use iced::{
    Color, Element, Length,
    widget::{
        svg::{self, Handle as SvgHandle, Svg},
        text,
    },
};

use crate::render::fonts;

/// Icon available to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Icon {
    Disc,
    Collection,
    Folder,
    Faders,
    FastForward,
    Gear,
    Home,
    Instrument,
    Lock,
    MusicNote,
    Pause,
    Play,
    PlayReverse,
    PlaylistAdd,
    Playlist,
    Plus,
    RepeatOnce,
    Repeat,
    Rewind,
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
    ZoomIn,
    ZoomOut,
}

enum IconSource {
    Lucide(lucide_icons::Icon),
    Svg(&'static [u8]),
}

impl Icon {
    /// Renders this icon with the given size and color.
    #[must_use]
    pub fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        match source(self) {
            IconSource::Lucide(icon) => text(char::from(icon).to_string())
                .font(fonts::LUCIDE)
                .size(size)
                .color(color)
                .into(),
            IconSource::Svg(bytes) => Svg::new(SvgHandle::from_memory(bytes))
                .width(Length::Fixed(size))
                .height(Length::Fixed(size))
                .style(move |_theme, _status| svg::Style { color: Some(color) })
                .into(),
        }
    }
}

fn source(icon: Icon) -> IconSource {
    match icon {
        Icon::FastForward => IconSource::Lucide(lucide_icons::Icon::FastForward),
        Icon::Rewind => IconSource::Lucide(lucide_icons::Icon::Rewind),
        Icon::ZoomIn => IconSource::Lucide(lucide_icons::Icon::ZoomIn),
        Icon::ZoomOut => IconSource::Lucide(lucide_icons::Icon::ZoomOut),
        Icon::Disc => IconSource::Svg(include_bytes!("../../assets/icons/disc.svg")),
        Icon::Collection => IconSource::Svg(include_bytes!("../../assets/icons/collection.svg")),
        Icon::Folder => IconSource::Svg(include_bytes!("../../assets/icons/folder.svg")),
        Icon::Faders => IconSource::Svg(include_bytes!("../../assets/icons/faders.svg")),
        Icon::Gear => IconSource::Svg(include_bytes!("../../assets/icons/gear.svg")),
        Icon::Home => IconSource::Svg(include_bytes!("../../assets/icons/home.svg")),
        Icon::Instrument => IconSource::Svg(include_bytes!("../../assets/icons/instrument.svg")),
        Icon::Lock => IconSource::Svg(include_bytes!("../../assets/icons/lock.svg")),
        Icon::MusicNote => IconSource::Svg(include_bytes!("../../assets/icons/music-note.svg")),
        Icon::Pause => IconSource::Svg(include_bytes!("../../assets/icons/pause.svg")),
        Icon::Play => IconSource::Svg(include_bytes!("../../assets/icons/play.svg")),
        Icon::PlayReverse => IconSource::Svg(include_bytes!("../../assets/icons/play-reverse.svg")),
        Icon::PlaylistAdd => IconSource::Svg(include_bytes!("../../assets/icons/playlist-add.svg")),
        Icon::Playlist => IconSource::Svg(include_bytes!("../../assets/icons/playlist.svg")),
        Icon::Plus => IconSource::Svg(include_bytes!("../../assets/icons/plus.svg")),
        Icon::RepeatOnce => IconSource::Svg(include_bytes!("../../assets/icons/repeat-once.svg")),
        Icon::Repeat => IconSource::Svg(include_bytes!("../../assets/icons/repeat.svg")),
        Icon::Shuffle => IconSource::Svg(include_bytes!("../../assets/icons/shuffle.svg")),
        Icon::SkipBack => IconSource::Svg(include_bytes!("../../assets/icons/skip-back.svg")),
        Icon::SkipForward => IconSource::Svg(include_bytes!("../../assets/icons/skip-forward.svg")),
        Icon::SpeakerHigh => IconSource::Svg(include_bytes!("../../assets/icons/speaker-high.svg")),
        Icon::SpeakerLow => IconSource::Svg(include_bytes!("../../assets/icons/speaker-low.svg")),
        Icon::SpeakerX => IconSource::Svg(include_bytes!("../../assets/icons/speaker-x.svg")),
        Icon::Search => IconSource::Svg(include_bytes!("../../assets/icons/search.svg")),
        Icon::Charts => IconSource::Svg(include_bytes!("../../assets/icons/charts.svg")),
        Icon::Clock => IconSource::Svg(include_bytes!("../../assets/icons/clock.svg")),
        Icon::Monitor => IconSource::Svg(include_bytes!("../../assets/icons/monitor.svg")),
        Icon::Usb => IconSource::Svg(include_bytes!("../../assets/icons/usb.svg")),
        Icon::Waveform => IconSource::Svg(include_bytes!("../../assets/icons/waveform.svg")),
        Icon::Zvuk => IconSource::Svg(include_bytes!("../../assets/icons/zvuk.svg")),
    }
}
