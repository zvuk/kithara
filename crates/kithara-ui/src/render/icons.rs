use std::sync::OnceLock;

#[cfg(feature = "iced")]
use iced::{
    Color, Element, Length,
    widget::{
        svg::{self, Handle as SvgHandle, Svg},
        text,
    },
};

use crate::{draw::Outline, module::IconName, render::model::TreeIcon};

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

pub(crate) fn document_icon(icon: IconName) -> Icon {
    match icon {
        IconName::Activity => Icon::Activity,
        IconName::Bell => Icon::Bell,
        IconName::ChevronDown => Icon::ChevronDown,
        IconName::ChevronRight => Icon::ChevronRight,
        IconName::ChevronUp => Icon::ChevronUp,
        IconName::Circle => Icon::Circle,
        IconName::Clock => Icon::Clock,
        IconName::Crown => Icon::Crown,
        IconName::Disc => Icon::Disc,
        IconName::Faders => Icon::Faders,
        IconName::FastForward => Icon::FastForward,
        IconName::FolderPlus => Icon::FolderPlus,
        IconName::Gear => Icon::Gear,
        IconName::Headphones => Icon::Headphones,
        IconName::Lock => Icon::Lock,
        IconName::LockOpen => Icon::LockOpen,
        IconName::Maximize => Icon::Maximize,
        IconName::Menu => Icon::Menu,
        IconName::Monitor => Icon::Monitor,
        IconName::Orbit => Icon::Orbit,
        IconName::Play => Icon::Play,
        IconName::PlayReverse => Icon::PlayReverse,
        IconName::Playlist => Icon::Playlist,
        IconName::Plus => Icon::Plus,
        IconName::Radio => Icon::Radio,
        IconName::RefreshCw => Icon::RefreshCw,
        IconName::Rewind => Icon::Rewind,
        IconName::Save => Icon::Save,
        IconName::SlidersHorizontal => Icon::SlidersHorizontal,
        IconName::SpeakerHigh => Icon::SpeakerHigh,
        IconName::Waveform => Icon::Waveform,
        IconName::X => Icon::X,
        IconName::ZoomIn => Icon::ZoomIn,
        IconName::ZoomOut => Icon::ZoomOut,
    }
}

pub(crate) fn tree_icon(icon: TreeIcon) -> Icon {
    match icon {
        TreeIcon::Collection => Icon::Collection,
        TreeIcon::Playlist => Icon::Playlist,
        TreeIcon::Folder => Icon::Folder,
        TreeIcon::Plus => Icon::Plus,
        TreeIcon::Zvuk => Icon::Zvuk,
        TreeIcon::Search => Icon::Search,
        TreeIcon::Charts => Icon::Charts,
        TreeIcon::Monitor => Icon::Monitor,
        TreeIcon::Home => Icon::Home,
        TreeIcon::Usb => Icon::Usb,
        TreeIcon::Instrument => Icon::Instrument,
        TreeIcon::Waveform => Icon::Waveform,
        TreeIcon::Clock => Icon::Clock,
    }
}

enum IconSource {
    Lucide(lucide_icons::Icon),
    Svg(&'static Art),
}

/// One icon's authored art, read into an outline the first time it is asked
/// for and kept, because a control asks for it once a frame.
struct Art {
    document: &'static str,
    outline: OnceLock<Option<Outline>>,
}

impl Art {
    const fn new(document: &'static str) -> Self {
        Self {
            document,
            outline: OnceLock::new(),
        }
    }

    fn outline(&'static self) -> Option<&'static Outline> {
        self.outline
            .get_or_init(|| match crate::draw::outline(self.document) {
                Ok(outline) => Some(outline),
                Err(error) => {
                    tracing::error!(%error, "an icon's art is not an outline this can draw");
                    None
                }
            })
            .as_ref()
    }
}

mod art {
    use super::Art;

    pub(super) static PLAY_REVERSE: Art =
        Art::new(include_str!("../../assets/icons/play-reverse.svg"));
    pub(super) static ZVUK: Art = Art::new(include_str!("../../assets/icons/zvuk.svg"));
}

/// What an icon is made of, once its source has been resolved.
///
/// Both halves reach the draw list: a glyph as shaped text, an outline as a
/// filled path. Neither needs a toolkit of its own.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mark {
    Glyph(char),
    Outline(&'static Outline),
}

impl Icon {
    pub(crate) fn lucide_glyph(self) -> Option<char> {
        match source(self) {
            IconSource::Lucide(icon) => Some(char::from(icon)),
            IconSource::Svg(_) => None,
        }
    }

    /// What this icon draws, or nothing when its art could not be read.
    pub(crate) fn mark(self) -> Option<Mark> {
        match source(self) {
            IconSource::Lucide(icon) => Some(Mark::Glyph(char::from(icon))),
            IconSource::Svg(art) => art.outline().map(Mark::Outline),
        }
    }

    /// Renders this icon with the given size and color.
    #[must_use]
    #[cfg(feature = "iced")]
    pub fn view<'a, M: 'a>(self, size: f32, color: Color) -> Element<'a, M> {
        match source(self) {
            IconSource::Lucide(icon) => text(char::from(icon).to_string())
                .font(crate::render::fonts::LUCIDE)
                .size(size)
                .color(color)
                .into(),
            IconSource::Svg(art) => Svg::new(SvgHandle::from_memory(art.document.as_bytes()))
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
        Icon::Bell => IconSource::Lucide(lucide_icons::Icon::Bell),
        Icon::Charts => IconSource::Lucide(lucide_icons::Icon::TrendingUp),
        Icon::ChevronDown => IconSource::Lucide(lucide_icons::Icon::ChevronDown),
        Icon::ChevronRight => IconSource::Lucide(lucide_icons::Icon::ChevronRight),
        Icon::ChevronUp => IconSource::Lucide(lucide_icons::Icon::ChevronUp),
        Icon::ChevronsLeft => IconSource::Lucide(lucide_icons::Icon::ChevronsLeft),
        Icon::ChevronsRight => IconSource::Lucide(lucide_icons::Icon::ChevronsRight),
        Icon::Circle => IconSource::Lucide(lucide_icons::Icon::Circle),
        Icon::Crown => IconSource::Lucide(lucide_icons::Icon::Crown),
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
        Icon::LockOpen => IconSource::Lucide(lucide_icons::Icon::LockOpen),
        Icon::Maximize => IconSource::Lucide(lucide_icons::Icon::Maximize),
        Icon::Menu => IconSource::Lucide(lucide_icons::Icon::Menu),
        Icon::Monitor => IconSource::Lucide(lucide_icons::Icon::Monitor),
        Icon::MusicNote => IconSource::Lucide(lucide_icons::Icon::Music),
        Icon::Orbit => IconSource::Lucide(lucide_icons::Icon::Orbit),
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
        Icon::PlayReverse => IconSource::Svg(&art::PLAY_REVERSE),
        Icon::Zvuk => IconSource::Svg(&art::ZVUK),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use lucide_icons::Icon as Lucide;

    use super::{Icon, Mark};
    use crate::draw::{FillRule, Rect};

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
                icon.lucide_glyph(),
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
                icon.lucide_glyph(),
                Some(char::from(wrong)),
                "{icon:?} must not be substituted by {wrong:?}"
            );
        }
        for (icon, right) in canon {
            assert_eq!(
                icon.lucide_glyph(),
                Some(char::from(right)),
                "{icon:?} must render {right:?}"
            );
        }
    }

    #[kithara::test]
    fn svg_icons_do_not_cross_the_glyph_seam() {
        assert_eq!(Icon::PlayReverse.lucide_glyph(), None);
        assert_eq!(Icon::Zvuk.lucide_glyph(), None);
    }

    /// Both authored icons read as outlines, so neither has to reach a toolkit
    /// to be seen. One of them fills with the even-odd rule and would be a solid
    /// blob without it, which is why the rule is asserted rather than assumed.
    #[kithara::test]
    fn authored_icons_read_as_outlines_this_can_draw() {
        let box_of = Rect {
            h: 1.0,
            w: 1.0,
            x: 0.0,
            y: 0.0,
        };
        for icon in [Icon::PlayReverse, Icon::Zvuk] {
            let Some(Mark::Outline(outline)) = icon.mark() else {
                panic!("{icon:?} must read as an outline");
            };
            assert!(
                outline.placed(box_of).verbs().len() > 4,
                "{icon:?} must carry its whole shape"
            );
        }

        let Some(Mark::Outline(zvuk)) = Icon::Zvuk.mark() else {
            panic!("the mark must be an outline");
        };
        assert_eq!(zvuk.placed(box_of).rule(), FillRule::EvenOdd);
    }
}
