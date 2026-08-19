use crate::{
    atoms::{icon::mark::Marked, painter::NavData},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, FontFamily, FontWeight, NavSkin, TextRoleSkin},
};

pub(crate) struct NavItem {
    active: Face,
    idle: Face,
    metrics: NavSkin,
    role: TextRoleSkin,
}

/// How the item looks in one of its two states. Both are resolved from the skin
/// when the item is built, so the page it points at becoming the current one is
/// a paint-time choice rather than a reason to rebuild the control.
struct Face {
    background: Rgba,
    content: Rgba,
    marker: Rgba,
}

impl NavItem {
    pub(crate) fn new(skin: &Skin) -> Self {
        let transparent = Rgba {
            a: 0.0,
            b: 0.0,
            g: 0.0,
            r: 0.0,
        };
        Self {
            active: Face {
                background: skin.palette.bg_select,
                content: skin.palette.text,
                marker: skin.palette.accent,
            },
            idle: Face {
                background: transparent,
                content: skin.palette.text_dim,
                marker: transparent,
            },
            metrics: skin.nav,
            role: TextRoleSkin {
                color: ColorRole::Text,
                font: FontFamily::Mono,
                size: skin.nav.text_size,
                spacing: 0.0,
                weight: FontWeight::Normal,
            },
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &NavData,
        bounds: Rect,
    ) {
        let face = if data.active {
            &self.active
        } else {
            &self.idle
        };
        let marker = self.marker(bounds);
        list.fill_rect(bounds, face.background);
        list.fill_rect(marker, face.marker);
        self.paint_content(list, text, data, face.content, bounds, marker);
    }

    fn marker(&self, bounds: Rect) -> Rect {
        let padding = self.metrics.pad_y;
        let inner_width = (bounds.w - padding * 2.0).max(0.0);
        Rect {
            h: (bounds.h - padding * 2.0).max(0.0),
            w: self.metrics.marker_width.max(0.0).min(inner_width),
            x: bounds.x + padding,
            y: bounds.y + padding,
        }
    }

    fn paint_content(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &NavData,
        content: Rgba,
        bounds: Rect,
        marker: Rect,
    ) {
        let x = marker.x + marker.w + self.metrics.text_pad_x;
        let width =
            Marked::new(data.mark, self.metrics.icon_size).paint(list, text, x, bounds, content);

        if data.label.is_empty() {
            return;
        }
        let run = text.shape(&data.label, self.role, None);
        list.text(
            &run,
            &data.label,
            Transform::translate(Pt {
                x: x + width + self.metrics.icon_gap,
                y: bounds.y + (bounds.h - run.height()) / 2.0,
            }),
            content,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Paint},
        render::Mark,
        shaping::{FontId, GlyphFace, GlyphSegment},
    };

    fn data(mark: Mark, active: bool) -> NavData {
        NavData {
            active,
            label: "PRIMITIVES".to_owned(),
            mark,
        }
    }

    #[kithara::test]
    fn an_active_nav_item_draws_its_background_marker_icon_and_label_in_order() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 198.0,
            x: 3.0,
            y: 5.0,
        };
        let mark = Mark::Glyph(char::from(lucide_icons::Icon::Disc));
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        NavItem::new(skin).paint(&mut builder, &mut text, &data(mark, true), bounds);
        let list = builder.finish();

        let [background, marker, icon, label] = list.commands() else {
            panic!("an active nav item must emit four drawing commands");
        };
        assert!(matches!(
            background,
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if *rect == bounds && *color == skin.palette.bg_select
        ));
        assert!(matches!(
            marker,
            DrawCmd::Fill {
                geom: Geom::Rect(Rect {
                    h: 30.0,
                    w: 2.0,
                    x: 3.0,
                    y: 5.0,
                }),
                paint: Paint::Solid(color),
            } if *color == skin.palette.accent
        ));
        let DrawCmd::Text {
            run: icon_run,
            content: icon_content,
            transform: icon_transform,
            color: icon_color,
        } = icon
        else {
            panic!("the third command must draw the nav icon");
        };
        assert_eq!(
            icon_run.segments().first().map(GlyphSegment::face),
            Some(&GlyphFace::Embedded(FontId::Lucide))
        );
        let Mark::Glyph(glyph) = mark else {
            panic!("the fixture mark is a glyph");
        };
        assert_eq!(icon_content, &glyph.to_string());
        assert_eq!(icon_transform.dx, 19.0);
        assert_eq!(
            icon_transform.dy,
            bounds.y + (bounds.h - icon_run.height()) / 2.0
        );
        assert_eq!(*icon_color, skin.palette.text);

        let DrawCmd::Text {
            run: label_run,
            content: label_content,
            transform: label_transform,
            color: label_color,
        } = label
        else {
            panic!("the fourth command must draw the nav label");
        };
        assert_eq!(
            label_run.segments().first().map(GlyphSegment::face),
            Some(&GlyphFace::Embedded(FontId::JetBrainsMonoRegular))
        );
        assert_eq!(label_content, "PRIMITIVES");
        assert_eq!(
            label_transform.dx,
            icon_transform.dx + icon_run.width() + 8.0
        );
        assert_eq!(
            label_transform.dy,
            bounds.y + (bounds.h - label_run.height()) / 2.0
        );
        assert_eq!(*label_color, skin.palette.text);
    }

    #[kithara::test]
    fn an_inactive_nav_item_keeps_both_rectangles_clear_and_dims_its_content() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut builder = DrawListBuilder::default();
        NavItem::new(skin).paint(
            &mut builder,
            &mut text,
            &data(Mark::Glyph(char::from(lucide_icons::Icon::Disc)), false),
            Rect {
                h: 30.0,
                w: 198.0,
                x: 0.0,
                y: 0.0,
            },
        );
        let list = builder.finish();
        let [
            DrawCmd::Fill {
                paint: Paint::Solid(background),
                ..
            },
            DrawCmd::Fill {
                paint: Paint::Solid(marker),
                ..
            },
            DrawCmd::Text {
                color: icon_color, ..
            },
            DrawCmd::Text {
                color: label_color, ..
            },
        ] = list.commands()
        else {
            panic!("an inactive nav item must emit two fills and two text commands");
        };

        assert_eq!(background.a, 0.0);
        assert_eq!(marker.a, 0.0);
        assert_eq!(*icon_color, skin.palette.text_dim);
        assert_eq!(*label_color, skin.palette.text_dim);
    }

    /// Turning to another page is a repaint, not a rebuild, so one mounted item
    /// must be able to draw both states.
    #[kithara::test]
    fn one_nav_item_draws_both_states() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 30.0,
            w: 198.0,
            x: 0.0,
            y: 0.0,
        };
        let item = NavItem::new(skin);
        let mark = Mark::Glyph(char::from(lucide_icons::Icon::Disc));
        let mut text = TextContext::from(skin.text_resources());
        let mut draw = |active| {
            let mut builder = DrawListBuilder::default();
            item.paint(&mut builder, &mut text, &data(mark, active), bounds);
            builder.finish()
        };

        assert_ne!(
            draw(true),
            draw(false),
            "the same nav item must draw differently once it is told it is current"
        );
    }
}
