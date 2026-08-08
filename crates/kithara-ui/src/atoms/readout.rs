use crate::{
    atoms::design::quad::border,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    module::Tone,
    render::Skin,
    skin::{FontFamily, ReadoutSkin, TextRoleSkin},
    text::TextContext,
};

/// A caption stacked over the value it names, framed or bare.
pub(crate) struct Readout {
    framed: bool,
    label_color: Rgba,
    label_role: TextRoleSkin,
    metrics: ReadoutSkin,
    stroke: Rgba,
    value_color: Rgba,
    value_role: TextRoleSkin,
}

/// What a readout is handed each frame: the word it names and the word it
/// shows.
pub(crate) struct ReadoutData {
    pub(crate) label: String,
    pub(crate) value: String,
}

impl Readout {
    pub(crate) fn new(tone: Tone, framed: bool, skin: &Skin) -> Self {
        let metrics = skin.readout;
        let role = |font: crate::skin::FontSkin, color| TextRoleSkin {
            color,
            font: FontFamily::Mono,
            size: font.size,
            spacing: 0.0,
            weight: font.weight,
        };
        let value_color = match tone {
            Tone::Neutral => skin.palette.text,
            Tone::Accent => skin.palette.accent,
            Tone::Success => skin.palette.success,
            Tone::Danger => skin.palette.danger,
        };
        Self {
            framed,
            label_color: skin.palette.muted,
            label_role: role(metrics.label, crate::skin::ColorRole::Muted),
            metrics,
            stroke: skin.rgba(metrics.frame.border),
            value_color,
            value_role: role(metrics.value, crate::skin::ColorRole::Text),
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &ReadoutData,
        bounds: Rect,
    ) {
        if self.framed {
            border(list, bounds, self.metrics.frame, self.stroke);
        }
        let x = bounds.x
            + if self.framed {
                self.metrics.padding_x
            } else {
                0.0
            };
        let label = text.shape(&data.label, self.label_role, None);
        let value = text.shape(&data.value, self.value_role, None);
        let stacked = label.height() + self.metrics.spacing + value.height();
        let y = bounds.y + (bounds.h - stacked) / 2.0;
        list.text(
            &label,
            &data.label,
            Transform::translate(Pt { x, y }),
            self.label_color,
        );
        list.text(
            &value,
            &data.value,
            Transform::translate(Pt {
                x,
                y: y + label.height() + self.metrics.spacing,
            }),
            self.value_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Readout, ReadoutData, Rect, TextContext, Tone};
    use crate::{builtin, draw::DrawCmd};

    const BOUNDS: Rect = Rect {
        h: 40.0,
        w: 90.0,
        x: 2.0,
        y: 3.0,
    };

    fn data() -> ReadoutData {
        ReadoutData {
            label: "GAIN".to_owned(),
            value: "0.50".to_owned(),
        }
    }

    fn drawn(tone: Tone, framed: bool) -> crate::draw::DrawList {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Readout::new(tone, framed, skin).paint(&mut list, &mut text, &data(), BOUNDS);
        list.finish()
    }

    /// The caption sits above the value rather than beside it, which is what
    /// makes a readout a readout.
    #[kithara::test]
    fn the_caption_sits_above_the_value_it_names() {
        let list = drawn(Tone::Neutral, false);
        let [
            DrawCmd::Text {
                transform: label, ..
            },
            DrawCmd::Text {
                transform: value, ..
            },
        ] = list.commands()
        else {
            panic!("a bare readout must draw its caption and its value and nothing else");
        };

        assert!(label.dy < value.dy);
    }

    #[kithara::test]
    fn the_tone_reaches_the_value_and_not_the_caption() {
        assert_ne!(drawn(Tone::Danger, false), drawn(Tone::Success, false));
    }

    /// A framed readout draws its frame and indents its words clear of it.
    #[kithara::test]
    fn a_framed_readout_draws_a_frame_the_bare_one_does_not() {
        assert_eq!(drawn(Tone::Neutral, false).commands().len(), 2);
        assert_eq!(drawn(Tone::Neutral, true).commands().len(), 3);
    }
}
