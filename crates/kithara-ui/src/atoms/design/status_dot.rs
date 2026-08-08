use crate::{
    atoms::design::quad::center_y,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::Tone,
    render::Skin,
    skin::{FontFamily, StatusDotSkin, TextRoleSkin},
    text::TextContext,
};

/// A coloured dot with a word beside it.
pub(crate) struct StatusDot {
    dot: Rgba,
    metrics: StatusDotSkin,
    role: TextRoleSkin,
    text: Rgba,
}

impl StatusDot {
    pub(crate) fn new(tone: Tone, skin: &Skin) -> Self {
        let metrics = skin.status_dot;
        Self {
            dot: match tone {
                Tone::Neutral => skin.palette.muted,
                Tone::Accent => skin.palette.accent,
                Tone::Success => skin.palette.success,
                Tone::Danger => skin.palette.danger,
            },
            metrics,
            role: TextRoleSkin {
                color: metrics.text_color,
                font: FontFamily::Mono,
                size: metrics.text.size,
                spacing: 0.0,
                weight: metrics.text.weight,
            },
            text: skin.rgba(metrics.text_color),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
    ) {
        let radius = self.metrics.dot_size / 2.0;
        list.fill_circle(
            Pt {
                x: bounds.x + radius,
                y: bounds.y + bounds.h / 2.0,
            },
            radius,
            self.dot,
        );
        let run = text.shape(label, self.role, None);
        list.text(
            &run,
            label,
            Transform::translate(Pt {
                x: bounds.x + self.metrics.dot_size + self.metrics.gap,
                y: center_y(bounds, &run),
            }),
            self.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Rect, StatusDot, TextContext, Tone};
    use crate::{
        builtin,
        draw::{DrawCmd, Geom, Paint},
    };

    /// The tone is the whole point of the control, and the caption clears the
    /// dot rather than sitting under it.
    #[kithara::test]
    fn the_dot_carries_the_tone_and_the_caption_clears_it() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 18.0,
            w: 64.0,
            x: 3.0,
            y: 5.0,
        };
        let draw = |tone| {
            let mut text = TextContext::from(skin.text_resources());
            let mut list = DrawListBuilder::default();
            StatusDot::new(tone, skin).paint(&mut list, &mut text, "LIVE", bounds);
            list.finish()
        };

        let list = draw(Tone::Danger);
        let [dot, caption] = list.commands() else {
            panic!("a status dot must draw its dot followed by its caption");
        };
        assert!(matches!(
            dot,
            DrawCmd::Fill {
                geom: Geom::Circle { center, radius },
                paint: Paint::Solid(color),
            } if *color == skin.palette.danger
                && *radius == skin.status_dot.dot_size / 2.0
                && center.x == bounds.x + *radius
                && center.y == bounds.y + bounds.h / 2.0
        ));
        assert!(matches!(
            caption,
            DrawCmd::Text {
                content, transform, ..
            } if content == "LIVE"
                && transform.dx == bounds.x + skin.status_dot.dot_size + skin.status_dot.gap
        ));

        assert_ne!(
            draw(Tone::Danger),
            draw(Tone::Success),
            "the tone must reach the dot"
        );
    }
}
