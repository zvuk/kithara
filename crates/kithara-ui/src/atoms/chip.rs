use crate::{
    atoms::design::quad::border,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::ChipStyle,
    render::Skin,
    shaping::TextContext,
    skin::{ColorRole, FontFamily, FontSkin, FrameSkin, TextRoleSkin},
};

pub(crate) struct Chip {
    active: Face,
    idle: Face,
    padding: Pt,
    role: TextRoleSkin,
}

/// How the chip looks in one of its two states. Both are resolved from the skin
/// when the chip is built, so switching states is a paint-time choice rather
/// than a reason to rebuild the control.
struct Face {
    border: Rgba,
    fill: Rgba,
    frame: FrameSkin,
    text: Rgba,
}

impl Chip {
    pub(crate) fn new(style: ChipStyle, skin: &Skin) -> Self {
        let (role, padding, active_frame, idle_frame, active_border) = match style {
            ChipStyle::Deck | ChipStyle::Routing => {
                let font: FontSkin = if style == ChipStyle::Deck {
                    skin.chip.deck_text
                } else {
                    skin.chip.routing_text
                };
                (
                    TextRoleSkin {
                        color: ColorRole::Text,
                        font: FontFamily::Mono,
                        size: font.size,
                        spacing: 0.0,
                        weight: font.weight,
                    },
                    Pt {
                        x: skin.chip.padding_x,
                        y: skin.chip.padding_y,
                    },
                    skin.chip.active_frame,
                    skin.chip.inactive_frame,
                    skin.rgba(skin.chip.active_frame.border),
                )
            }
            ChipStyle::PivotFamily => (
                skin.chip.pivot_family_text,
                Pt {
                    x: skin.chip.pivot_family_padding_x,
                    y: skin.chip.pivot_family_padding_y,
                },
                skin.chip.pivot_frame,
                skin.chip.pivot_frame,
                skin.palette.accent,
            ),
            ChipStyle::PivotMultiplier => (
                skin.chip.pivot_multiplier_text,
                Pt {
                    x: skin.chip.pivot_multiplier_padding_x,
                    y: skin.chip.pivot_multiplier_padding_y,
                },
                skin.chip.pivot_frame,
                skin.chip.pivot_frame,
                skin.palette.accent,
            ),
        };
        Self {
            active: Face {
                border: active_border,
                fill: skin.palette.accent,
                frame: active_frame,
                text: skin.palette.bg_deep,
            },
            idle: Face {
                border: skin.rgba(idle_frame.border),
                fill: Rgba {
                    a: 0.0,
                    b: 0.0,
                    g: 0.0,
                    r: 0.0,
                },
                frame: idle_frame,
                text: skin.palette.text_dim,
            },
            padding,
            role,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        active: bool,
        bounds: Rect,
    ) {
        let face = if active { &self.active } else { &self.idle };
        list.fill_rounded_rect(bounds, face.frame.radius, face.fill);
        border(list, bounds, face.frame, face.border);
        let run = text.shape(label, self.role, None);
        list.text(
            &run,
            label,
            Transform::translate(Pt {
                x: bounds.x + self.padding.x,
                y: bounds.y + self.padding.y,
            }),
            face.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        builtin,
        draw::{DrawCmd, DrawListBuilder, Geom, Paint, Pen, Rect},
        shaping::TextContext,
    };

    #[kithara::test]
    fn chip_paints_its_state_frame_and_shaped_label_in_order() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 16.0,
            w: 28.0,
            x: 3.0,
            y: 5.0,
        };
        let draw = |label, style, active| {
            let mut text = TextContext::from(skin.text_resources());
            let mut builder = DrawListBuilder::default();
            Chip::new(style, skin).paint(&mut builder, &mut text, label, active, bounds);
            builder.finish()
        };

        let active = draw("A", ChipStyle::Deck, true);
        let [fill, label] = active.commands() else {
            panic!("an active chip must draw its fill followed by its label");
        };
        assert!(matches!(
            fill,
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if *rect == bounds && *color == skin.palette.accent
        ));
        assert!(matches!(
            label,
            DrawCmd::Text {
                run,
                content,
                color,
                transform,
                ..
            } if content == "A"
                && run.size() == skin.chip.deck_text.size
                && transform.dx == 11.0
                && transform.dy == 8.0
                && *color == skin.palette.bg_deep
        ));

        let inactive = draw("FX1", ChipStyle::Routing, false);
        let [fill, frame, label] = inactive.commands() else {
            panic!("an inactive chip must draw its clear fill, frame, then label");
        };
        assert!(matches!(
            fill,
            DrawCmd::Fill {
                geom: Geom::Rect(rect),
                paint: Paint::Solid(color),
            } if *rect == bounds && color.a == 0.0
        ));
        assert!(matches!(
            frame,
            DrawCmd::Stroke {
                geom: Geom::Rect(Rect {
                    h: 15.0,
                    w: 27.0,
                    x: 3.5,
                    y: 5.5,
                }),
                color,
                pen: Pen { width: 1.0, .. },
            } if *color == skin.rgba(skin.chip.inactive_frame.border)
        ));
        assert!(matches!(
            label,
            DrawCmd::Text {
                run,
                content,
                color,
                transform,
                ..
            } if content == "FX1"
                && run.size() == skin.chip.routing_text.size
                && transform.dx == 11.0
                && transform.dy == 8.0
                && *color == skin.palette.text_dim
        ));
    }

    /// The two states differ only in how the chip is painted, so one chip must
    /// be able to draw both without being rebuilt.
    #[kithara::test]
    fn one_chip_draws_both_states() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 16.0,
            w: 28.0,
            x: 0.0,
            y: 0.0,
        };
        let chip = Chip::new(ChipStyle::Deck, skin);
        let mut text = TextContext::from(skin.text_resources());
        let mut draw = |active| {
            let mut builder = DrawListBuilder::default();
            chip.paint(&mut builder, &mut text, "A", active, bounds);
            builder.finish()
        };

        assert_ne!(
            draw(true),
            draw(false),
            "the same chip must draw differently once it is told it is active"
        );
    }
}
