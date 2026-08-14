use iced::{
    Color, Element, Length,
    widget::{
        svg::{self, Handle as SvgHandle, Svg},
        text,
    },
};
use lucide_icons::Icon as LucideIcon;

use crate::render::fonts;

/// Icon available to renderers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Icon {
    Activity,
    Bell,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    ChevronsLeft,
    ChevronsRight,
    Circle,
    Crown,
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
    LockOpen,
    Maximize,
    Menu,
    MusicNote,
    Orbit,
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
    Lucide(LucideIcon),
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

const fn source(icon: Icon) -> IconSource {
    match icon {
        Icon::Activity => IconSource::Lucide(LucideIcon::Activity),
        Icon::Bell => IconSource::Lucide(LucideIcon::Bell),
        Icon::Charts => IconSource::Lucide(LucideIcon::TrendingUp),
        Icon::ChevronDown => IconSource::Lucide(LucideIcon::ChevronDown),
        Icon::ChevronRight => IconSource::Lucide(LucideIcon::ChevronRight),
        Icon::ChevronUp => IconSource::Lucide(LucideIcon::ChevronUp),
        Icon::ChevronsLeft => IconSource::Lucide(LucideIcon::ChevronsLeft),
        Icon::ChevronsRight => IconSource::Lucide(LucideIcon::ChevronsRight),
        Icon::Circle => IconSource::Lucide(LucideIcon::Circle),
        Icon::Crown => IconSource::Lucide(LucideIcon::Crown),
        Icon::Clock => IconSource::Lucide(LucideIcon::Clock),
        Icon::Collection => IconSource::Lucide(LucideIcon::CircleDot),
        Icon::Disc => IconSource::Lucide(LucideIcon::Disc),
        Icon::Faders => IconSource::Lucide(LucideIcon::Sliders),
        Icon::FastForward => IconSource::Lucide(LucideIcon::FastForward),
        Icon::Folder => IconSource::Lucide(LucideIcon::Folder),
        Icon::FolderPlus => IconSource::Lucide(LucideIcon::FolderPlus),
        Icon::Gear => IconSource::Lucide(LucideIcon::Settings),
        Icon::Headphones => IconSource::Lucide(LucideIcon::Headphones),
        Icon::Home => IconSource::Lucide(LucideIcon::Home),
        Icon::Instrument => IconSource::Lucide(LucideIcon::KeyboardMusic),
        Icon::Lock => IconSource::Lucide(LucideIcon::Lock),
        Icon::LockOpen => IconSource::Lucide(LucideIcon::LockOpen),
        Icon::Maximize => IconSource::Lucide(LucideIcon::Maximize),
        Icon::Menu => IconSource::Lucide(LucideIcon::Menu),
        Icon::Monitor => IconSource::Lucide(LucideIcon::Monitor),
        Icon::MusicNote => IconSource::Lucide(LucideIcon::Music),
        Icon::Orbit => IconSource::Lucide(LucideIcon::Orbit),
        Icon::Pause => IconSource::Lucide(LucideIcon::Pause),
        Icon::Play => IconSource::Lucide(LucideIcon::Play),
        Icon::Playlist => IconSource::Lucide(LucideIcon::ListMusic),
        Icon::PlaylistAdd => IconSource::Lucide(LucideIcon::ListPlus),
        Icon::Plus => IconSource::Lucide(LucideIcon::Plus),
        Icon::Radio => IconSource::Lucide(LucideIcon::Radio),
        Icon::RefreshCw => IconSource::Lucide(LucideIcon::RefreshCw),
        Icon::Repeat => IconSource::Lucide(LucideIcon::Repeat),
        Icon::RepeatOnce => IconSource::Lucide(LucideIcon::Repeat1),
        Icon::Rewind => IconSource::Lucide(LucideIcon::Rewind),
        Icon::Save => IconSource::Lucide(LucideIcon::Save),
        Icon::Search => IconSource::Lucide(LucideIcon::Search),
        Icon::Shuffle => IconSource::Lucide(LucideIcon::Shuffle),
        Icon::SkipBack => IconSource::Lucide(LucideIcon::SkipBack),
        Icon::SkipForward => IconSource::Lucide(LucideIcon::SkipForward),
        Icon::SlidersHorizontal => IconSource::Lucide(LucideIcon::SlidersHorizontal),
        Icon::SpeakerHigh => IconSource::Lucide(LucideIcon::Volume2),
        Icon::SpeakerLow => IconSource::Lucide(LucideIcon::Volume1),
        Icon::SpeakerX => IconSource::Lucide(LucideIcon::VolumeX),
        Icon::Usb => IconSource::Lucide(LucideIcon::Usb),
        Icon::Waveform => IconSource::Lucide(LucideIcon::AudioWaveform),
        Icon::X => IconSource::Lucide(LucideIcon::X),
        Icon::ZoomIn => IconSource::Lucide(LucideIcon::ZoomIn),
        Icon::ZoomOut => IconSource::Lucide(LucideIcon::ZoomOut),
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
