use std::borrow::Cow;

use crate::{
    draw::{Rgba, TRANSPARENT},
    module::{TextStyle, Tone, text_roles},
    render::Skin,
    shaping::{GlyphRun, TextContext},
    skin::{ColorRole, FontFamily, FontWeight, TextCase, TextRoleSkin, TextSkin, ToneColors},
};

/// The one rule that picks between a node's own colour and its active one.
///
/// A node is active or it is not, and the active role only wins while it is;
/// a node naming no active role keeps the base one it declared.
pub(crate) fn active_tone(
    base: Option<ColorRole>,
    active: Option<ColorRole>,
    on: bool,
) -> Option<ColorRole> {
    on.then_some(active).flatten().or(base)
}

/// The role one tone names in a control's own tone set.
pub(crate) const fn tone_color(tone: Tone, tones: ToneColors) -> ColorRole {
    match tone {
        Tone::Accent => tones.accent,
        Tone::Danger => tones.danger,
        Tone::Neutral => tones.neutral,
        Tone::Success => tones.success,
    }
}

impl Skin {
    pub(crate) fn rgba(&self, role: ColorRole) -> Rgba {
        self.palette[role]
    }
    /// The typography one document text style names, with the tone already
    /// selected.
    ///
    /// Both hosts ask the skin rather than keeping a table each: a style the
    /// two answered differently would paint the same document in two
    /// typefaces, which is the one thing the shared base exists to prevent.
    /// There is no wildcard arm, so a new style does not build until it is
    /// given a skin entry.
    pub(crate) fn text_role(
        &self,
        style: TextStyle,
        color: Option<ColorRole>,
        active_color: Option<ColorRole>,
        active: bool,
    ) -> TextRoleSkin {
        let role = self.text.role(style);
        let skin_active = (style == TextStyle::DeckLetter).then_some(self.text.deck_letter_active);
        TextRoleSkin {
            color: active_tone(color, active_color.or(skin_active), active).unwrap_or(role.color),
            ..role
        }
    }
    /// Resolves one state of a [`StateColors`]: a state naming no role paints
    /// nothing.
    pub(crate) fn tint(&self, role: Option<ColorRole>) -> Rgba {
        role.map_or(TRANSPARENT, |role| self.rgba(role))
    }
}

impl TextRoleSkin {
    /// The words this role sets, in its case.
    ///
    /// Every host asks here rather than deciding for itself, because the case a
    /// run is set in changes how wide it is, and two hosts that answered
    /// separately would lay the same document out differently.
    pub(crate) fn cased(self, content: &str) -> Cow<'_, str> {
        match self.case {
            TextCase::Upper => Cow::Owned(content.to_uppercase()),
            TextCase::AsWritten => Cow::Borrowed(content),
        }
    }

    /// The line this role sets in `max_width`. A layout box is whole pixels, so
    /// a line less than a pixel wider than its box was snapped down from its
    /// natural size and stays whole.
    pub(crate) fn fit<'a>(
        self,
        text: &mut TextContext,
        content: &'a str,
        max_width: f32,
    ) -> (Cow<'a, str>, GlyphRun) {
        let max_width = max_width + 1.0;
        match self.elide {
            Some(at) => text.shape_elided(content, self, max_width, at),
            None => (
                Cow::Borrowed(content),
                text.shape(content, self, Some(max_width)),
            ),
        }
    }

    /// The size this role sets `content` at in a box `room` wide, padding
    /// aside: its one line where that fits, and where it does not, the whole
    /// room and the height of the lines it wraps to. A role that elides keeps
    /// its one line, and is cut to the box where it paints.
    ///
    /// Both hosts measure here, so a paragraph takes the same box on either,
    /// and [`Self::fit`] never sets more lines than this counted.
    pub(crate) fn measure(self, text: &mut TextContext, content: &str, room: f32) -> (f32, f32) {
        let line = text.shape(content, self, None);
        if self.elide.is_some() || line.width() <= room {
            return (line.width(), line.height());
        }
        (room, text.shape(content, self, Some(room)).height())
    }

    /// This role set in the face a run names, keeping the skin's where it
    /// names none.
    pub(crate) fn faced(self, font: Option<FontFamily>, weight: Option<FontWeight>) -> Self {
        Self {
            font: font.unwrap_or(self.font),
            weight: weight.unwrap_or(self.weight),
            ..self
        }
    }
}

macro_rules! define_text_role {
    ($($(#[$attr:meta])* $field:ident => $role:ident),* $(,)?) => {
        impl TextSkin {
            /// The entry this skin gives one typographic role.
            pub(crate) fn role(&self, style: TextStyle) -> TextRoleSkin {
                match style {
                    $(TextStyle::$role => self.$field,)*
                }
            }
        }
    };
}

text_roles!(define_text_role);

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{builtin, module::TextStyle, skin::ColorRole};

    #[kithara::test]
    fn a_role_sets_its_words_in_the_case_the_skin_names() {
        let skin = builtin::skin();

        assert_eq!(skin.text.micro_label.cased("0.0.1-alpha4"), "0.0.1-ALPHA4");
        assert_eq!(skin.text.cell.cased("Zvuk"), "ZVUK");
        assert_eq!(skin.text.body.cased("0.0.1-alpha4"), "0.0.1-alpha4");
    }

    #[kithara::test]
    fn an_eliding_role_keeps_a_long_line_to_one_line_in_its_box() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources.as_ref());
        let path = "/Users/someone/Library/Application Support/kithara/config.toml";
        let width = text.shape(path, skin.text.note, None).width() / 2.0;

        let (shown, run) = skin.text.note.fit(&mut text, path, width);
        assert!(run.width() <= width + 1.0);
        assert_eq!(run.height(), text.shape("~", skin.text.note, None).height());
        assert!(shown.ends_with("config.toml"), "{shown}");
        let (_, wrapped) = skin.text.body.fit(&mut text, path, width);
        assert!(wrapped.height() > run.height());
    }

    #[kithara::test]
    fn a_node_colour_stands_in_for_the_one_the_role_carries() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Mono, Some(ColorRole::Text), None, false),
            TextRoleSkin {
                color: ColorRole::Text,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn a_node_switches_between_the_two_colours_it_names() {
        let skin = builtin::skin();
        let role = |active| {
            skin.text_role(
                TextStyle::Mono,
                Some(ColorRole::Muted),
                Some(ColorRole::Accent),
                active,
            )
        };

        assert_eq!(
            role(true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.mono
            }
        );
        assert_eq!(
            role(false),
            TextRoleSkin {
                color: ColorRole::Muted,
                ..skin.text.mono
            }
        );
    }

    #[kithara::test]
    fn an_active_node_naming_one_colour_keeps_it() {
        let skin = builtin::skin();

        assert_eq!(
            skin.text_role(TextStyle::Caption, Some(ColorRole::Accent), None, true),
            TextRoleSkin {
                color: ColorRole::Accent,
                ..skin.text.caption
            }
        );
    }

    #[kithara::test]
    fn the_deck_letter_takes_the_active_colour_its_skin_entry_declares() {
        let skin = builtin::skin();
        let base = skin.text_role(TextStyle::DeckLetter, None, None, false);

        assert_eq!(base, skin.text.deck_letter);
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, None, true),
            TextRoleSkin {
                color: skin.text.deck_letter_active,
                ..base
            }
        );
        assert_eq!(
            skin.text_role(TextStyle::DeckLetter, None, Some(ColorRole::Warning), true),
            TextRoleSkin {
                color: ColorRole::Warning,
                ..base
            }
        );
    }

    #[kithara::test]
    fn brand_small_resolves_under_the_display_family_and_never_the_mono_one() {
        let skin = builtin::skin();
        let role = skin.text_role(TextStyle::BrandSmall, None, None, false);

        assert_eq!(role, skin.text.brand_small);
        assert_eq!(
            skin.text_role(TextStyle::BrandSmall, None, None, true),
            role
        );
        assert_ne!(
            role.font, skin.text.mono.font,
            "the mono micro roles are Mono and the brand pair is Display"
        );
    }

    #[kithara::test]
    fn a_style_declaring_no_active_colour_ignores_the_flag() {
        let skin = builtin::skin();

        for style in [
            TextStyle::Body,
            TextStyle::Brand,
            TextStyle::TrackTitle,
            TextStyle::Telemetry,
            TextStyle::MicroLabel,
            TextStyle::Section,
            TextStyle::Mono,
            TextStyle::PivotArrow,
            TextStyle::PivotTitle,
            TextStyle::PivotValue,
            TextStyle::Caption,
            TextStyle::VisFooter,
            TextStyle::VisMeta,
            TextStyle::VisTitle,
        ] {
            assert_eq!(
                skin.text_role(style, None, None, true),
                skin.text_role(style, None, None, false),
                "{style:?}"
            );
        }
    }

    #[kithara::test]
    fn active_tone_takes_the_active_role_only_while_the_flag_is_set() {
        let pair =
            |active| active_tone(Some(ColorRole::LineInner), Some(ColorRole::Accent), active);

        assert_eq!(pair(true), Some(ColorRole::Accent));
        assert_eq!(pair(false), Some(ColorRole::LineInner));
        assert_eq!(
            active_tone(Some(ColorRole::LineHi), None, true),
            Some(ColorRole::LineHi)
        );
        assert_eq!(active_tone(None, None, true), None);
    }
}
