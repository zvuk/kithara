use bon::Builder;

use crate::{
    expand::Binding,
    module::{GlyphStyle, IconName},
    mount::Control,
    size::{Dim, SizeSpec},
    skin::{ColorRole, SkinDoc},
};

/// A single icon, drawn as a text glyph.
#[derive(Builder)]
pub(crate) struct Glyph<'a> {
    pub(crate) active: Option<&'a Binding>,
    pub(crate) active_color: Option<ColorRole>,
    pub(crate) active_icon: Option<IconName>,
    pub(crate) color: Option<ColorRole>,
    pub(crate) icon: IconName,
    pub(crate) style: GlyphStyle,
}

impl Control for Glyph<'_> {
    fn size(&self, skin: &SkinDoc) -> SizeSpec {
        match self.style {
            GlyphStyle::Menu => cell(skin.menu.icon_size),
            GlyphStyle::MenuBurger => cell(skin.menu.burger_icon_size),
            GlyphStyle::MenuSmall => cell(skin.menu.small_icon_size),
            GlyphStyle::MenuCell => cell(skin.menu.cell_icon_size),
            GlyphStyle::Default | GlyphStyle::Vis => SizeSpec::new(
                Dim::Fixed(skin.nav.header_icon_size),
                Dim::Fixed(skin.nav.header_height),
            ),
        }
    }
}

/// An icon renders as a text glyph, whose line box is taller than the icon
/// size; the row it sits in owns the height so the glyph centres against its
/// siblings.
fn cell(side: f32) -> SizeSpec {
    SizeSpec::new(Dim::Fixed(side), Dim::Fill)
}

#[cfg(feature = "render")]
mod host {
    use super::Glyph;
    use crate::{
        atoms::icon::glyph::{Glyph as Face, GlyphData},
        draw::Rgba,
        module::GlyphStyle,
        render::{
            Icon, ReadValue, Skin,
            controls::{Draws, Reading},
            document::read::resolve,
            document_icon,
        },
        skin::ColorRole,
    };

    impl Draws for Glyph<'_> {
        type Painter = Face;

        fn painter(&self, skin: &Skin) -> Face {
            let base = base(self.style, skin);
            Face::new(
                self.color.map_or(base, |role| skin.rgba(role)),
                self.active_color
                    .or(self.color)
                    .map_or(base, |role| skin.rgba(role)),
                size(self.style, skin),
            )
        }

        /// A glyph is nothing but its art, so one whose art could not be read
        /// draws nothing rather than a hole in the row.
        ///
        /// The flag it shows comes from an endpoint of its own rather than from
        /// the control's value, so it is read here.
        fn data(&self, read: Reading<'_>) -> Option<GlyphData> {
            Some(GlyphData {
                active: self.active.is_some_and(|binding| {
                    matches!(
                        resolve(read.reads, binding, read.ui),
                        Some(ReadValue::Bool(true))
                    )
                }),
                active_mark: self.active_icon.map(document_icon).and_then(Icon::mark),
                mark: document_icon(self.icon).mark()?,
            })
        }
    }

    /// The colour a glyph takes when the document names none.
    fn base(style: GlyphStyle, skin: &Skin) -> Rgba {
        match style {
            GlyphStyle::Vis => skin.rgba(skin.vis.icon_color),
            GlyphStyle::Default
            | GlyphStyle::Menu
            | GlyphStyle::MenuBurger
            | GlyphStyle::MenuSmall
            | GlyphStyle::MenuCell => skin.rgba(ColorRole::Text),
        }
    }

    fn size(style: GlyphStyle, skin: &Skin) -> f32 {
        match style {
            GlyphStyle::Default => skin.nav.header_icon_size,
            GlyphStyle::Vis => skin.vis.icon_size,
            GlyphStyle::Menu => skin.menu.icon_size,
            GlyphStyle::MenuBurger => skin.menu.burger_icon_size,
            GlyphStyle::MenuSmall => skin.menu.small_icon_size,
            GlyphStyle::MenuCell => skin.menu.cell_icon_size,
        }
    }

    #[cfg(test)]
    mod tests {
        use kithara_test_utils::kithara;

        use super::{ColorRole, Face, Glyph, GlyphData, GlyphStyle, base, size};
        use crate::{
            builtin,
            draw::{DrawList, DrawListBuilder, Rect},
            module::IconName,
            render::{Icon, Skin, controls::Draws},
            text::TextContext,
        };

        const BOUNDS: Rect = Rect {
            h: 24.0,
            w: 24.0,
            x: 0.0,
            y: 0.0,
        };

        #[kithara::test]
        fn every_glyph_style_takes_its_own_skin_icon_size() {
            let skin = builtin::skin();

            for (style, expected) in [
                (GlyphStyle::Default, skin.nav.header_icon_size),
                (GlyphStyle::Vis, skin.vis.icon_size),
                (GlyphStyle::Menu, skin.menu.icon_size),
                (GlyphStyle::MenuBurger, skin.menu.burger_icon_size),
                (GlyphStyle::MenuSmall, skin.menu.small_icon_size),
                (GlyphStyle::MenuCell, skin.menu.cell_icon_size),
            ] {
                assert_eq!(size(style, skin), expected, "{style:?}");
            }
        }

        /// A document that names both colours gets both, so the two states do
        /// not draw the same picture.
        #[kithara::test]
        fn a_declared_glyph_pair_switches_on_the_active_flag() {
            let painter = painter(Some(ColorRole::Muted), Some(ColorRole::Danger));

            assert_ne!(drawn(&painter, false), drawn(&painter, true));
        }

        /// Naming one colour and no second one means the glyph keeps it while
        /// active rather than falling back to what its style would have given.
        #[kithara::test]
        fn an_active_glyph_naming_no_active_colour_keeps_its_base() {
            let painter = painter(Some(ColorRole::Accent), None);

            assert_eq!(drawn(&painter, false), drawn(&painter, true));
        }

        #[kithara::test]
        fn a_glyph_naming_no_colour_leaves_the_tone_to_its_style() {
            let skin = builtin::skin();
            let tone = base(GlyphStyle::Default, skin);

            assert_eq!(
                drawn(&painter(None, None), false),
                drawn(
                    &Face::new(tone, tone, size(GlyphStyle::Default, skin)),
                    false
                )
            );
        }

        #[kithara::test]
        fn the_vis_style_is_the_one_that_does_not_take_the_text_colour() {
            let skin = builtin::skin();

            assert_eq!(base(GlyphStyle::Vis, skin), skin.rgba(skin.vis.icon_color));

            for style in [
                GlyphStyle::Default,
                GlyphStyle::Menu,
                GlyphStyle::MenuBurger,
                GlyphStyle::MenuSmall,
                GlyphStyle::MenuCell,
            ] {
                assert_eq!(base(style, skin), skin.rgba(ColorRole::Text), "{style:?}");
            }
        }

        fn painter(color: Option<ColorRole>, active_color: Option<ColorRole>) -> Face {
            Glyph::builder()
                .maybe_color(color)
                .maybe_active_color(active_color)
                .icon(IconName::Play)
                .style(GlyphStyle::Default)
                .build()
                .painter(builtin::skin())
        }

        fn drawn(painter: &Face, active: bool) -> DrawList {
            let skin: &Skin = builtin::skin();
            let mut list = DrawListBuilder::default();
            painter.paint(
                &mut list,
                &mut TextContext::from(skin.text_resources()),
                &GlyphData {
                    active,
                    active_mark: None,
                    mark: Icon::Play
                        .mark()
                        .unwrap_or_else(|| panic!("the play icon must have a mark")),
                },
                BOUNDS,
            );
            list.finish()
        }
    }
}
