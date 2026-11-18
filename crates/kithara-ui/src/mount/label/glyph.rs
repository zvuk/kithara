use crate::{
    module::GlyphStyle,
    mount::Control,
    size::{Dim, SizeSpec},
    skin::SkinDoc,
};

/// A single icon, drawn as a text glyph.
pub(crate) struct Glyph {
    pub(crate) style: GlyphStyle,
}

impl Control for Glyph {
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

#[cfg(any(feature = "iced", feature = "masonry"))]
pub(crate) mod host {
    use bon::Builder;

    use crate::{
        expand::{Binding, ColorSpec},
        skin::ColorRole,
    };

    #[derive(Builder)]
    pub(crate) struct Glyph<'a> {
        pub(crate) style: GlyphStyle,
        pub(crate) icon: IconName,
        pub(crate) active: Option<&'a Binding>,
        pub(crate) active_color: Option<ColorRole>,
        pub(crate) active_icon: Option<IconName>,
        pub(crate) color: Option<&'a ColorSpec>,
    }

    use crate::{
        atoms::icon::glyph::{Glyph as Face, GlyphData},
        hosts::{
            controls::{Draws, Reading},
            icons::Mark,
            skin::active_tone,
        },
        module::{GlyphStyle, IconName},
        render::{Skin, theme::RenderPalette},
    };

    pub(crate) struct GlyphAppearance {
        idle: Mark,
        active: Option<Mark>,
        idle_role: ColorRole,
        active_role: Option<ColorRole>,
    }

    impl GlyphAppearance {
        pub(crate) fn data(
            &self,
            on: bool,
            color: Option<ColorRole>,
            palette: &RenderPalette,
        ) -> GlyphData {
            let mark = if on {
                self.active.unwrap_or(self.idle)
            } else {
                self.idle
            };
            let role = active_tone(color, self.active_role, on).unwrap_or(self.idle_role);
            GlyphData {
                mark,
                color: palette[role],
            }
        }
    }

    impl Glyph<'_> {
        pub(crate) fn appearance(&self, skin: &Skin) -> Option<GlyphAppearance> {
            Some(GlyphAppearance {
                idle: self.icon.mark()?,
                active: self.active_icon.and_then(IconName::mark),
                idle_role: base(self.style, skin),
                active_role: self.active_color,
            })
        }
    }

    impl Draws for Glyph<'_> {
        type Painter = Face;

        fn data(&self, read: Reading<'_>) -> Option<GlyphData> {
            Some(self.appearance(read.skin)?.data(
                read.ctx.flag(self.active),
                read.ctx.color_role(self.color),
                &read.skin.palette,
            ))
        }

        fn painter(&self, skin: &Skin) -> Face {
            Face::new(size(self.style, skin))
        }
    }

    /// The colour a glyph takes when the document names none.
    fn base(style: GlyphStyle, skin: &Skin) -> ColorRole {
        match style {
            GlyphStyle::Vis => skin.vis.icon_color,
            GlyphStyle::Default => skin.nav.header_icon_color,
            GlyphStyle::Menu
            | GlyphStyle::MenuBurger
            | GlyphStyle::MenuSmall
            | GlyphStyle::MenuCell => skin.menu.icon_color,
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
        use std::collections::BTreeMap;

        use kithara_test_utils::kithara;

        use super::{Face, Glyph, GlyphData, GlyphStyle, base, size};
        use crate::{
            builtin,
            compile::CompiledNode,
            draw::{DrawList, DrawListBuilder, Rect},
            expand::{Binding, BindingKind, ColorSpec},
            hosts::controls::{Draws, Reading},
            module::IconName,
            render::{ReadValue, Reads, Skin, document::probe},
            shaping::TextContext,
            skin::ColorRole,
        };

        struct Flag(bool);

        impl Reads for Flag {
            fn get(&self, _endpoint: &str) -> Option<ReadValue<'_>> {
                Some(ReadValue::Bool(self.0))
            }
        }

        mod consts {
            use super::*;

            pub(super) const BOUNDS: Rect = Rect {
                h: 24.0,
                w: 24.0,
                x: 0.0,
                y: 0.0,
            };
        }

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
            assert_ne!(
                drawn(Some(ColorRole::Muted), Some(ColorRole::Danger), false),
                drawn(Some(ColorRole::Muted), Some(ColorRole::Danger), true)
            );
        }

        /// Naming one colour and no second one means the glyph keeps it while
        /// active rather than falling back to what its style would have given.
        #[kithara::test]
        fn an_active_glyph_naming_no_active_colour_keeps_its_base() {
            assert_eq!(
                drawn(Some(ColorRole::Accent), None, false),
                drawn(Some(ColorRole::Accent), None, true)
            );
        }

        #[kithara::test]
        fn a_glyph_naming_no_colour_leaves_the_tone_to_its_style() {
            let skin = builtin::skin();
            let tone = skin.rgba(base(GlyphStyle::Default, skin));
            let mut list = DrawListBuilder::default();
            Face::new(size(GlyphStyle::Default, skin)).paint(
                &mut list,
                &mut TextContext::from(skin.text_resources.as_ref()),
                &GlyphData {
                    color: tone,
                    mark: IconName::Play
                        .mark()
                        .expect("the play icon must have a mark"),
                },
                consts::BOUNDS,
            );

            assert_eq!(drawn(None, None, false), list.finish());
        }

        #[kithara::test]
        fn the_vis_style_is_the_one_that_does_not_take_the_text_colour() {
            let skin = builtin::skin();

            assert_eq!(base(GlyphStyle::Vis, skin), skin.vis.icon_color);

            for style in [
                GlyphStyle::Default,
                GlyphStyle::Menu,
                GlyphStyle::MenuBurger,
                GlyphStyle::MenuSmall,
                GlyphStyle::MenuCell,
            ] {
                assert_eq!(base(style, skin), ColorRole::Text, "{style:?}");
            }
        }

        fn drawn(
            color: Option<ColorRole>,
            active_color: Option<ColorRole>,
            active: bool,
        ) -> DrawList {
            let skin: &Skin = builtin::skin();
            let spec = color.map(ColorSpec::Role);
            let flag = Flag(active);
            let ctx = probe(&flag);
            let CompiledNode::Module { instance, .. } = &ctx.ui.root else {
                panic!("probe module")
            };
            let binding = Binding {
                id: *instance,
                key: *instance,
                kind: BindingKind::Telemetry,
                with: BTreeMap::new(),
            };
            let glyph = Glyph::builder()
                .active(&binding)
                .maybe_color(spec.as_ref())
                .maybe_active_color(active_color)
                .icon(IconName::Play)
                .style(GlyphStyle::Default)
                .build();
            let painter = glyph.painter(skin);
            let data = glyph
                .data(Reading {
                    ctx,
                    scope: "",
                    value: None,
                    skin,
                })
                .expect("play icon");
            let mut list = DrawListBuilder::default();
            painter.paint(
                &mut list,
                &mut TextContext::from(skin.text_resources.as_ref()),
                &data,
                consts::BOUNDS,
            );
            list.finish()
        }
    }
}
