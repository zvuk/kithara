use bon::Builder;

use crate::{
    ids::InternId,
    layout::FrameSides,
    module::{ButtonStyle, IconName},
    mount::Control,
    size::{Dim, SizeSpec},
    skin::SkinDoc,
};

/// A pressable button, worded and optionally iconed by the document.
#[derive(Builder)]
pub(crate) struct Button {
    pub(crate) active_label: Option<InternId>,
    pub(crate) frame: Option<FrameSides>,
    pub(crate) icon: Option<IconName>,
    pub(crate) label: InternId,
    pub(crate) style: ButtonStyle,
}

impl Control for Button {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        match self.style {
            ButtonStyle::VisNav => square(skin.vis.nav_cell_size),
            ButtonStyle::MicroPrimary => square(skin.button.micro_size),
            ButtonStyle::Default | ButtonStyle::Transport | ButtonStyle::TransportPrimary => {
                skin.button.size
            }
        }
    }
}

fn square(side: f32) -> SizeSpec {
    SizeSpec::new(Dim::Fixed(side), Dim::Fixed(side))
}

#[cfg(feature = "render")]
mod host {
    use super::Button;
    use crate::{
        atoms::{
            button::{Button as Face, ButtonConfig, ButtonLabel},
            painter::ButtonData,
        },
        module::ButtonStyle,
        render::{
            Icon, Mark, ReadValue, Skin,
            controls::{Draws, Grip, Reading},
            document_icon,
        },
    };

    /// The icon a button shows at rest, and the one it swaps in while active.
    /// One style changes its icon along with its word, so both are resolved
    /// together and a button that flips repaints instead of being rebuilt.
    struct Marks {
        active: Option<Mark>,
        idle: Option<Mark>,
    }

    /// What a button draws once its style has had its say about the document's
    /// icon. The style that names its own icon answers differently for each
    /// state; the rest answer the same either way.
    fn mark(style: ButtonStyle, icon: Option<Icon>, active: bool) -> Option<Mark> {
        if style == ButtonStyle::MicroPrimary {
            return if active { Icon::Pause } else { Icon::Play }.mark();
        }
        icon.and_then(Icon::mark)
    }

    impl Draws for Button {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            let icon = self.icon.map(document_icon);
            let marks = Marks {
                active: mark(self.style, icon, true),
                idle: mark(self.style, icon, false),
            };
            Face::new(
                ButtonConfig::builder()
                    .maybe_frame(self.frame)
                    .maybe_mark(marks.idle)
                    .style(self.style)
                    .build(),
                marks.active,
                skin,
            )
        }

        /// A button always draws. Unlike a switch, one whose endpoint has not
        /// spoken is a button at rest rather than an empty box — it still shows
        /// its word, and pressing it is how the endpoint first gets a value.
        fn data(&self, read: Reading<'_>) -> Option<ButtonData> {
            Some(ButtonData {
                active: matches!(read.value, Some(ReadValue::Bool(true))),
                label: ButtonLabel {
                    active: self
                        .active_label
                        .map(|label| read.ui.resolve(label).to_owned()),
                    label: read.ui.resolve(self.label).to_owned(),
                },
            })
        }

        fn grip(&self, _skin: &Skin, _data: &ButtonData) -> Grip {
            Grip::Press
        }
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::{ButtonStyle, Icon, Mark, mark};

        /// The one style that names its own icon keeps naming it, and every
        /// other style draws the authored art rather than handing it to a
        /// toolkit.
        #[kithara::test]
        fn micro_primary_keeps_its_forced_lucide_icon() {
            assert!(matches!(
                mark(ButtonStyle::MicroPrimary, Some(Icon::PlayReverse), false),
                Some(Mark::Glyph(glyph)) if Some(glyph) == Icon::Play.lucide_glyph()
            ));
        }

        #[kithara::test]
        fn authored_art_reaches_a_button_as_an_outline() {
            assert!(matches!(
                mark(ButtonStyle::Default, Some(Icon::PlayReverse), false),
                Some(Mark::Outline(_))
            ));
        }
    }
}
