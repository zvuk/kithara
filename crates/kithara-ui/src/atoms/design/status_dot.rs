use crate::{
    atoms::design::quad::center_y,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::Tone,
    render::Skin,
    shaping::TextContext,
    skin::{FontFamily, StatusDotSkin, TextRoleSkin},
};

/// A coloured dot with a word beside it.
pub(crate) struct StatusDot {
    active_dot: Option<Rgba>,
    dot: Rgba,
    dot_size: f32,
    metrics: StatusDotSkin,
    role: TextRoleSkin,
    text: Rgba,
}

/// The caption and whether the document marks this dot active.
pub(crate) struct StatusDotData {
    pub(crate) active: bool,
    pub(crate) label: String,
}

impl StatusDot {
    pub(crate) fn with_active_tone(
        tone: Tone,
        active_tone: Option<Tone>,
        dot_size: Option<f32>,
        skin: &Skin,
    ) -> Self {
        let metrics = skin.status_dot;
        Self {
            active_dot: active_tone.map(|tone| color(tone, skin)),
            dot: color(tone, skin),
            dot_size: dot_size.unwrap_or(metrics.dot_size),
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

    /// How wide this dot needs to be to show what it draws: the dot itself, and
    /// the gap and the shaped word when it captions one.
    ///
    /// A document that asks for `Shrink` is asking exactly this. Without it the
    /// dot has no width of its own to give, and a host that believes the answer
    /// lays the control out to nothing.
    pub(crate) fn intrinsic_width(&self, text: &mut TextContext, label: &str) -> f32 {
        if label.is_empty() {
            return self.dot_size;
        }
        self.dot_size + self.metrics.gap + text.shape(label, self.role, None).width()
    }

    pub(crate) fn paint_with_state(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: &str,
        bounds: Rect,
        active: bool,
    ) {
        let radius = self.dot_size / 2.0;
        list.fill_circle(
            Pt {
                x: bounds.x + radius,
                y: bounds.y + bounds.h / 2.0,
            },
            radius,
            self.active_dot.filter(|_| active).unwrap_or(self.dot),
        );
        if label.is_empty() {
            return;
        }
        let run = text.shape(label, self.role, None);
        list.text(
            &run,
            label,
            Transform::translate(Pt {
                x: bounds.x + self.dot_size + self.metrics.gap,
                y: center_y(bounds, &run),
            }),
            self.text,
        );
    }
}

fn color(tone: Tone, skin: &Skin) -> Rgba {
    match tone {
        Tone::Neutral => skin.palette.muted,
        Tone::Accent => skin.palette.accent,
        Tone::Success => skin.palette.success,
        Tone::Danger => skin.palette.danger,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Rect, StatusDot, StatusDotData, TextContext, Tone};
    use crate::{
        atoms::painter::ControlPainter,
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
            StatusDot::with_active_tone(tone, None, None, skin)
                .paint_with_state(&mut list, &mut text, "LIVE", bounds, false);
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

        let active = StatusDot::with_active_tone(Tone::Neutral, Some(Tone::Danger), None, skin);
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        active.paint_with_state(&mut list, &mut text, "LIVE", bounds, true);
        assert!(matches!(
            list.finish().commands(),
            [DrawCmd::Fill { paint: Paint::Solid(color), .. }, ..] if *color == skin.palette.danger
        ));
    }

    /// A dot the document asks to shrink has to answer with the width it draws.
    ///
    /// Both hosts resolve `Shrink` against what the painter measures, and the
    /// measurement is what reaches them — not the helper beside it. A dot that
    /// answered nothing was laid out to nothing on the immediate host, and the
    /// bar drew an empty cell where the retained host drew `REC`.
    #[kithara::test]
    fn a_captioned_dot_measures_its_own_word() {
        let skin = builtin::skin();
        let dot = StatusDot::with_active_tone(Tone::Neutral, None, None, skin);
        let mut text = TextContext::from(skin.text_resources());
        let word = text.shape("REC", dot.role, None).width();

        let measured = ControlPainter::measure(&dot, &mut text, &captioned("REC"));

        assert_eq!(
            measured.width,
            skin.status_dot.dot_size + skin.status_dot.gap + word
        );
    }

    /// Without a word there is no gap to leave: the dot is the whole control.
    #[kithara::test]
    fn an_uncaptioned_dot_measures_the_dot_alone() {
        let skin = builtin::skin();
        let dot = StatusDot::with_active_tone(Tone::Neutral, None, None, skin);
        let mut text = TextContext::from(skin.text_resources());

        let measured = ControlPainter::measure(&dot, &mut text, &captioned(""));

        assert_eq!(measured.width, skin.status_dot.dot_size);
    }

    /// The height stays the row's to give: every dot in a bar lines up with its
    /// neighbours whatever word it carries.
    #[kithara::test]
    fn a_dot_leaves_its_height_to_the_row() {
        let skin = builtin::skin();
        let dot = StatusDot::with_active_tone(Tone::Neutral, None, None, skin);
        let mut text = TextContext::from(skin.text_resources());

        let measured = ControlPainter::measure(&dot, &mut text, &captioned("REC"));

        assert_eq!(measured.height, 0.0);
    }

    fn captioned(label: &str) -> StatusDotData {
        StatusDotData {
            active: false,
            label: label.to_owned(),
        }
    }
}
