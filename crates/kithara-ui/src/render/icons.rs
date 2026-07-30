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
    Activity,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    ChevronsLeft,
    ChevronsRight,
    Circle,
    Disc,
    Collection,
    Folder,
    FolderPlus,
    Faders,
    FastForward,
    Gear,
    Home,
    Headphones,
    Instrument,
    Lock,
    Maximize,
    Menu,
    MusicNote,
    Pause,
    Play,
    PlayReverse,
    PlaylistAdd,
    Playlist,
    Plus,
    Radio,
    RefreshCw,
    RepeatOnce,
    Repeat,
    Rewind,
    Save,
    Shuffle,
    SkipBack,
    SkipForward,
    SlidersHorizontal,
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
        Icon::Activity => IconSource::Lucide(lucide_icons::Icon::Activity),
        Icon::Charts => IconSource::Lucide(lucide_icons::Icon::TrendingUp),
        Icon::ChevronDown => IconSource::Lucide(lucide_icons::Icon::ChevronDown),
        Icon::ChevronRight => IconSource::Lucide(lucide_icons::Icon::ChevronRight),
        Icon::ChevronUp => IconSource::Lucide(lucide_icons::Icon::ChevronUp),
        Icon::ChevronsLeft => IconSource::Lucide(lucide_icons::Icon::ChevronsLeft),
        Icon::ChevronsRight => IconSource::Lucide(lucide_icons::Icon::ChevronsRight),
        Icon::Circle => IconSource::Lucide(lucide_icons::Icon::Circle),
        Icon::Clock => IconSource::Lucide(lucide_icons::Icon::Clock),
        Icon::Collection => IconSource::Lucide(lucide_icons::Icon::CircleDot),
        Icon::Disc => IconSource::Lucide(lucide_icons::Icon::Disc),
        Icon::Faders => IconSource::Lucide(lucide_icons::Icon::Sliders),
        Icon::FastForward => IconSource::Lucide(lucide_icons::Icon::FastForward),
        Icon::Folder => IconSource::Lucide(lucide_icons::Icon::Folder),
        Icon::FolderPlus => IconSource::Lucide(lucide_icons::Icon::FolderPlus),
        Icon::Gear => IconSource::Lucide(lucide_icons::Icon::Settings),
        Icon::Headphones => IconSource::Lucide(lucide_icons::Icon::Headphones),
        Icon::Home => IconSource::Lucide(lucide_icons::Icon::Home),
        Icon::Instrument => IconSource::Lucide(lucide_icons::Icon::KeyboardMusic),
        Icon::Lock => IconSource::Lucide(lucide_icons::Icon::Lock),
        Icon::Maximize => IconSource::Lucide(lucide_icons::Icon::Maximize),
        Icon::Menu => IconSource::Lucide(lucide_icons::Icon::Menu),
        Icon::Monitor => IconSource::Lucide(lucide_icons::Icon::Monitor),
        Icon::MusicNote => IconSource::Lucide(lucide_icons::Icon::Music),
        Icon::Pause => IconSource::Lucide(lucide_icons::Icon::Pause),
        Icon::Play => IconSource::Lucide(lucide_icons::Icon::Play),
        Icon::Playlist => IconSource::Lucide(lucide_icons::Icon::ListMusic),
        Icon::PlaylistAdd => IconSource::Lucide(lucide_icons::Icon::ListPlus),
        Icon::Plus => IconSource::Lucide(lucide_icons::Icon::Plus),
        Icon::Radio => IconSource::Lucide(lucide_icons::Icon::Radio),
        Icon::RefreshCw => IconSource::Lucide(lucide_icons::Icon::RefreshCw),
        Icon::Repeat => IconSource::Lucide(lucide_icons::Icon::Repeat),
        Icon::RepeatOnce => IconSource::Lucide(lucide_icons::Icon::Repeat1),
        Icon::Rewind => IconSource::Lucide(lucide_icons::Icon::Rewind),
        Icon::Save => IconSource::Lucide(lucide_icons::Icon::Save),
        Icon::Search => IconSource::Lucide(lucide_icons::Icon::Search),
        Icon::Shuffle => IconSource::Lucide(lucide_icons::Icon::Shuffle),
        Icon::SkipBack => IconSource::Lucide(lucide_icons::Icon::SkipBack),
        Icon::SkipForward => IconSource::Lucide(lucide_icons::Icon::SkipForward),
        Icon::SlidersHorizontal => IconSource::Lucide(lucide_icons::Icon::SlidersHorizontal),
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use lucide_icons::Icon as Lucide;

    use super::{Icon, IconSource, source};

    fn glyph(icon: Icon) -> Option<char> {
        match source(icon) {
            IconSource::Lucide(lucide) => Some(char::from(lucide)),
            IconSource::Svg(_) => None,
        }
    }

    #[kithara::test]
    fn every_app_menu_glyph_resolves_to_its_lucide_namesake() {
        let table: [(Icon, Lucide); 16] = [
            (Icon::Menu, Lucide::Menu),
            (Icon::X, Lucide::X),
            (Icon::ChevronDown, Lucide::ChevronDown),
            (Icon::Maximize, Lucide::Maximize),
            (Icon::Disc, Lucide::Disc),
            (Icon::Gear, Lucide::Settings),
            (Icon::Monitor, Lucide::Monitor),
            (Icon::Plus, Lucide::Plus),
            (Icon::RefreshCw, Lucide::RefreshCw),
            (Icon::ChevronRight, Lucide::ChevronRight),
            (Icon::Activity, Lucide::Activity),
            (Icon::SlidersHorizontal, Lucide::SlidersHorizontal),
            (Icon::Circle, Lucide::Circle),
            (Icon::Radio, Lucide::Radio),
            (Icon::FolderPlus, Lucide::FolderPlus),
            (Icon::Save, Lucide::Save),
        ];

        for (icon, lucide) in table {
            assert_eq!(
                glyph(icon),
                Some(char::from(lucide)),
                "{icon:?} must render {lucide:?}"
            );
        }
    }

    #[kithara::test]
    fn role_named_incumbents_keep_their_own_glyphs() {
        let prohibited: [(Icon, Lucide); 5] = [
            (Icon::Faders, Lucide::SlidersHorizontal),
            (Icon::Collection, Lucide::Circle),
            (Icon::ChevronsRight, Lucide::ChevronRight),
            (Icon::Waveform, Lucide::Activity),
            (Icon::Charts, Lucide::Activity),
        ];
        let canon: [(Icon, Lucide); 5] = [
            (Icon::Faders, Lucide::Sliders),
            (Icon::Collection, Lucide::CircleDot),
            (Icon::ChevronsRight, Lucide::ChevronsRight),
            (Icon::Waveform, Lucide::AudioWaveform),
            (Icon::Charts, Lucide::TrendingUp),
        ];

        for (icon, wrong) in prohibited {
            assert_ne!(
                glyph(icon),
                Some(char::from(wrong)),
                "{icon:?} must not be substituted by {wrong:?}"
            );
        }
        for (icon, right) in canon {
            assert_eq!(
                glyph(icon),
                Some(char::from(right)),
                "{icon:?} must render {right:?}"
            );
        }
    }
}
