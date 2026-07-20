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
    Menu,
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
    X,
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
        Icon::Charts => IconSource::Lucide(lucide_icons::Icon::TrendingUp),
        Icon::Clock => IconSource::Lucide(lucide_icons::Icon::Clock),
        Icon::Collection => IconSource::Lucide(lucide_icons::Icon::CircleDot),
        Icon::Disc => IconSource::Lucide(lucide_icons::Icon::Disc),
        Icon::Faders => IconSource::Lucide(lucide_icons::Icon::Sliders),
        Icon::FastForward => IconSource::Lucide(lucide_icons::Icon::FastForward),
        Icon::Folder => IconSource::Lucide(lucide_icons::Icon::Folder),
        Icon::Gear => IconSource::Lucide(lucide_icons::Icon::Settings),
        Icon::Home => IconSource::Lucide(lucide_icons::Icon::Home),
        Icon::Instrument => IconSource::Lucide(lucide_icons::Icon::KeyboardMusic),
        Icon::Lock => IconSource::Lucide(lucide_icons::Icon::Lock),
        Icon::Menu => IconSource::Lucide(lucide_icons::Icon::Menu),
        Icon::Monitor => IconSource::Lucide(lucide_icons::Icon::Monitor),
        Icon::MusicNote => IconSource::Lucide(lucide_icons::Icon::Music),
        Icon::Pause => IconSource::Lucide(lucide_icons::Icon::Pause),
        Icon::Play => IconSource::Lucide(lucide_icons::Icon::Play),
        Icon::Playlist => IconSource::Lucide(lucide_icons::Icon::ListMusic),
        Icon::PlaylistAdd => IconSource::Lucide(lucide_icons::Icon::ListPlus),
        Icon::Plus => IconSource::Lucide(lucide_icons::Icon::Plus),
        Icon::Repeat => IconSource::Lucide(lucide_icons::Icon::Repeat),
        Icon::RepeatOnce => IconSource::Lucide(lucide_icons::Icon::Repeat1),
        Icon::Rewind => IconSource::Lucide(lucide_icons::Icon::Rewind),
        Icon::Search => IconSource::Lucide(lucide_icons::Icon::Search),
        Icon::Shuffle => IconSource::Lucide(lucide_icons::Icon::Shuffle),
        Icon::SkipBack => IconSource::Lucide(lucide_icons::Icon::SkipBack),
        Icon::SkipForward => IconSource::Lucide(lucide_icons::Icon::SkipForward),
        Icon::SpeakerHigh => IconSource::Lucide(lucide_icons::Icon::Volume2),
        Icon::SpeakerLow => IconSource::Lucide(lucide_icons::Icon::Volume1),
        Icon::SpeakerX => IconSource::Lucide(lucide_icons::Icon::VolumeX),
        Icon::Usb => IconSource::Lucide(lucide_icons::Icon::Usb),
        Icon::Waveform => IconSource::Lucide(lucide_icons::Icon::AudioWaveform),
        Icon::X => IconSource::Lucide(lucide_icons::Icon::X),
        Icon::ZoomIn => IconSource::Lucide(lucide_icons::Icon::ZoomIn),
        Icon::ZoomOut => IconSource::Lucide(lucide_icons::Icon::ZoomOut),
        Icon::PlayReverse => IconSource::Svg(include_bytes!("../../assets/icons/play-reverse.svg")),
        Icon::Zvuk => IconSource::Svg(include_bytes!("../../assets/icons/zvuk.svg")),
    }
}
