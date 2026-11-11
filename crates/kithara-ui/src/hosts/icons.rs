use std::sync::OnceLock;

use lucide_icons::Icon;

use crate::{draw::Outline, module::IconName};

pub(crate) enum IconSource {
    Lucide(Icon),
    Svg(&'static Art),
}

/// One icon's authored art, read into an outline the first time it is asked
/// for and kept, because a control asks for it once a frame.
pub(crate) struct Art {
    pub(crate) document: &'static str,
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
    pub(super) static HEART_FILLED: Art =
        Art::new(include_str!("../../assets/icons/heart-filled.svg"));
    pub(super) static KITHARA: Art = Art::new(include_str!("../../assets/icons/kithara.svg"));
    pub(super) static ZVUK: Art = Art::new(include_str!("../../assets/icons/zvuk.svg"));
}

/// What an icon is made of, once its source has been resolved.
///
/// Both halves reach the draw list: a glyph as shaped text, an outline as a
/// filled path. Neither needs a toolkit of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mark {
    Glyph(char),
    Outline(&'static Outline),
}

impl IconName {
    /// What this icon draws, or nothing when its art could not be read.
    pub(crate) fn mark(self) -> Option<Mark> {
        match source(self) {
            IconSource::Lucide(icon) => Some(Mark::Glyph(char::from(icon))),
            IconSource::Svg(art) => art.outline().map(Mark::Outline),
        }
    }
}

pub(crate) fn source(icon: IconName) -> IconSource {
    match icon {
        IconName::Activity => IconSource::Lucide(Icon::Activity),
        IconName::Bell => IconSource::Lucide(Icon::Bell),
        IconName::Charts => IconSource::Lucide(Icon::TrendingUp),
        IconName::ChevronDown => IconSource::Lucide(Icon::ChevronDown),
        IconName::ChevronLeft => IconSource::Lucide(Icon::ChevronLeft),
        IconName::ChevronRight => IconSource::Lucide(Icon::ChevronRight),
        IconName::ChevronUp => IconSource::Lucide(Icon::ChevronUp),
        IconName::ChevronsLeft => IconSource::Lucide(Icon::ChevronsLeft),
        IconName::ChevronsRight => IconSource::Lucide(Icon::ChevronsRight),
        IconName::Circle => IconSource::Lucide(Icon::Circle),
        IconName::Crown => IconSource::Lucide(Icon::Crown),
        IconName::Clock => IconSource::Lucide(Icon::Clock),
        IconName::Collection => IconSource::Lucide(Icon::CircleDot),
        IconName::Disc => IconSource::Lucide(Icon::Disc),
        IconName::Faders => IconSource::Lucide(Icon::Sliders),
        IconName::FastForward => IconSource::Lucide(Icon::FastForward),
        IconName::Folder => IconSource::Lucide(Icon::Folder),
        IconName::FolderPlus => IconSource::Lucide(Icon::FolderPlus),
        IconName::Gear => IconSource::Lucide(Icon::Settings),
        IconName::Headphones => IconSource::Lucide(Icon::Headphones),
        IconName::Heart => IconSource::Lucide(Icon::Heart),
        IconName::HeartFilled => IconSource::Svg(&art::HEART_FILLED),
        IconName::Home => IconSource::Lucide(Icon::Home),
        IconName::Instrument => IconSource::Lucide(Icon::KeyboardMusic),
        IconName::Lock => IconSource::Lucide(Icon::Lock),
        IconName::LockOpen => IconSource::Lucide(Icon::LockOpen),
        IconName::Maximize => IconSource::Lucide(Icon::Maximize),
        IconName::Menu => IconSource::Lucide(Icon::Menu),
        IconName::Monitor => IconSource::Lucide(Icon::Monitor),
        IconName::Music2 => IconSource::Lucide(Icon::Music2),
        IconName::MusicNote => IconSource::Lucide(Icon::Music),
        IconName::Orbit => IconSource::Lucide(Icon::Orbit),
        IconName::Pause => IconSource::Lucide(Icon::Pause),
        IconName::Play => IconSource::Lucide(Icon::Play),
        IconName::Playlist => IconSource::Lucide(Icon::ListMusic),
        IconName::PlaylistAdd => IconSource::Lucide(Icon::ListPlus),
        IconName::Plus => IconSource::Lucide(Icon::Plus),
        IconName::Radio => IconSource::Lucide(Icon::Radio),
        IconName::RefreshCw => IconSource::Lucide(Icon::RefreshCw),
        IconName::Repeat => IconSource::Lucide(Icon::Repeat),
        IconName::RepeatOnce => IconSource::Lucide(Icon::Repeat1),
        IconName::Rewind => IconSource::Lucide(Icon::Rewind),
        IconName::Save => IconSource::Lucide(Icon::Save),
        IconName::Search => IconSource::Lucide(Icon::Search),
        IconName::Shuffle => IconSource::Lucide(Icon::Shuffle),
        IconName::SkipBack => IconSource::Lucide(Icon::SkipBack),
        IconName::SkipForward => IconSource::Lucide(Icon::SkipForward),
        IconName::SlidersHorizontal => IconSource::Lucide(Icon::SlidersHorizontal),
        IconName::SpeakerHigh => IconSource::Lucide(Icon::Volume2),
        IconName::SpeakerLow => IconSource::Lucide(Icon::Volume1),
        IconName::SpeakerX => IconSource::Lucide(Icon::VolumeX),
        IconName::Usb => IconSource::Lucide(Icon::Usb),
        IconName::Waveform => IconSource::Lucide(Icon::AudioWaveform),
        IconName::X => IconSource::Lucide(Icon::X),
        IconName::ZoomIn => IconSource::Lucide(Icon::ZoomIn),
        IconName::ZoomOut => IconSource::Lucide(Icon::ZoomOut),
        IconName::Kithara => IconSource::Svg(&art::KITHARA),
        IconName::PlayReverse => IconSource::Svg(&art::PLAY_REVERSE),
        IconName::Zvuk => IconSource::Svg(&art::ZVUK),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use lucide_icons::Icon as Lucide;

    use super::{IconSource, Mark};
    use crate::{
        draw::{FillRule, Rect},
        module::IconName,
    };

    #[kithara::test]
    fn each_listed_glyph_resolves_to_its_lucide_icon() {
        let table = [
            (IconName::Menu, Lucide::Menu),
            (IconName::X, Lucide::X),
            (IconName::ChevronDown, Lucide::ChevronDown),
            (IconName::Maximize, Lucide::Maximize),
            (IconName::Disc, Lucide::Disc),
            (IconName::Gear, Lucide::Settings),
            (IconName::Monitor, Lucide::Monitor),
            (IconName::Plus, Lucide::Plus),
            (IconName::RefreshCw, Lucide::RefreshCw),
            (IconName::ChevronRight, Lucide::ChevronRight),
            (IconName::Activity, Lucide::Activity),
            (IconName::SlidersHorizontal, Lucide::SlidersHorizontal),
            (IconName::Circle, Lucide::Circle),
            (IconName::Radio, Lucide::Radio),
            (IconName::FolderPlus, Lucide::FolderPlus),
            (IconName::Save, Lucide::Save),
            (IconName::Music2, Lucide::Music2),
            (IconName::ChevronLeft, Lucide::ChevronLeft),
        ];

        for (icon, lucide) in table {
            assert_eq!(
                icon.mark(),
                Some(Mark::Glyph(char::from(lucide))),
                "{icon:?} must render {lucide:?}"
            );
        }
    }

    #[kithara::test]
    fn role_named_incumbents_keep_their_own_glyphs() {
        let prohibited: [(IconName, Lucide); 5] = [
            (IconName::Faders, Lucide::SlidersHorizontal),
            (IconName::Collection, Lucide::Circle),
            (IconName::ChevronsRight, Lucide::ChevronRight),
            (IconName::Waveform, Lucide::Activity),
            (IconName::Charts, Lucide::Activity),
        ];
        let canon: [(IconName, Lucide); 5] = [
            (IconName::Faders, Lucide::Sliders),
            (IconName::Collection, Lucide::CircleDot),
            (IconName::ChevronsRight, Lucide::ChevronsRight),
            (IconName::Waveform, Lucide::AudioWaveform),
            (IconName::Charts, Lucide::TrendingUp),
        ];

        for (icon, wrong) in prohibited {
            assert_ne!(
                icon.mark(),
                Some(Mark::Glyph(char::from(wrong))),
                "{icon:?} must not be substituted by {wrong:?}"
            );
        }
        for (icon, right) in canon {
            assert_eq!(
                icon.mark(),
                Some(Mark::Glyph(char::from(right))),
                "{icon:?} must render {right:?}"
            );
        }
    }

    /// Every icon drawn from an authored SVG, taken from the one list of icon
    /// names rather than written out again here: a mark added to `source` and
    /// forgotten here would otherwise go unchecked.
    fn authored() -> Vec<IconName> {
        IconName::ALL
            .iter()
            .copied()
            .filter(|icon| matches!(super::source(*icon), IconSource::Svg(_)))
            .collect()
    }

    #[kithara::test]
    fn svg_icons_do_not_cross_the_glyph_seam() {
        for icon in authored() {
            assert!(
                !matches!(icon.mark(), Some(Mark::Glyph(_))),
                "{icon:?} draws its own art"
            );
        }
    }

    /// Every authored icon reads as an outline, so none has to reach a toolkit
    /// to be seen. One of them fills with the even-odd rule and would be a solid
    /// blob without it, which is why the rule is asserted rather than assumed.
    ///
    /// `Art` answers a document it cannot read with a log line and nothing
    /// drawn, so an icon that stops parsing is blank rather than loud. This is
    /// where that is caught.
    #[kithara::test]
    fn authored_icons_read_as_outlines_this_can_draw() {
        let box_of = Rect {
            h: 1.0,
            w: 1.0,
            x: 0.0,
            y: 0.0,
        };
        for icon in authored() {
            let Some(Mark::Outline(outline)) = icon.mark() else {
                panic!("{icon:?} must read as an outline");
            };
            assert!(
                outline.placed(box_of).verbs().len() > 4,
                "{icon:?} must carry its whole shape"
            );
        }

        let Some(Mark::Outline(zvuk)) = IconName::Zvuk.mark() else {
            panic!("the mark must be an outline");
        };
        assert_eq!(zvuk.placed(box_of).rule(), FillRule::EvenOdd);
    }
}
