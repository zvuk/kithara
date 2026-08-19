use crate::{
    atoms::design::quad::{center_y, quad},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    shaping::TextContext,
    skin::{FontFamily, SelectSkin, TextRoleSkin},
};

/// The chevron a select shows on its closing edge.
const CHEVRON: &str = "\u{2304}";

/// A framed box with a word on one edge and a chevron on the other.
pub(crate) struct Select {
    background: Rgba,
    chevron: Rgba,
    chevron_role: TextRoleSkin,
    metrics: SelectSkin,
    role: TextRoleSkin,
    stroke: Rgba,
    text: Rgba,
}

impl Select {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.select;
        let role = |size| TextRoleSkin {
            color: metrics.text_color,
            font: FontFamily::Mono,
            size,
            spacing: 0.0,
            weight: metrics.text.weight,
        };
        Self {
            background: skin.rgba(metrics.background),
            chevron: skin.rgba(metrics.chevron_color),
            chevron_role: role(metrics.chevron_size),
            metrics,
            role: role(metrics.text.size),
            stroke: skin.rgba(metrics.frame.border),
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
        quad(
            list,
            bounds,
            self.metrics.frame,
            self.background,
            self.stroke,
        );
        let run = text.shape(label, self.role, None);
        list.text(
            &run,
            label,
            Transform::translate(Pt {
                x: bounds.x + self.metrics.padding_x,
                y: center_y(bounds, &run),
            }),
            self.text,
        );
        let chevron = text.shape(CHEVRON, self.chevron_role, None);
        list.text(
            &chevron,
            CHEVRON,
            Transform::translate(Pt {
                x: bounds.x + bounds.w - self.metrics.padding_x - chevron.width(),
                y: center_y(bounds, &chevron),
            }),
            self.chevron,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{CHEVRON, DrawListBuilder, Rect, Select, TextContext};
    use crate::{builtin, draw::DrawCmd};

    const BOUNDS: Rect = Rect {
        h: 26.0,
        w: 160.0,
        x: 4.0,
        y: 6.0,
    };

    /// The word starts inside the padding on the opening edge and the chevron
    /// ends inside it on the closing one, so a long word cannot push the
    /// chevron out of the box.
    #[kithara::test]
    fn the_word_and_the_chevron_each_clear_the_padding_on_their_own_edge() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Select::new(skin).paint(&mut list, &mut text, "PRESET", BOUNDS);
        let list = list.finish();

        let [_, _, word, chevron] = list.commands() else {
            panic!("a select must draw its box, its frame, its word and its chevron");
        };
        assert!(matches!(
            word,
            DrawCmd::Text { content, transform, .. }
                if content == "PRESET" && transform.dx == BOUNDS.x + skin.select.padding_x
        ));
        assert!(matches!(
            chevron,
            DrawCmd::Text { content, transform, .. }
                if content == CHEVRON
                    && transform.dx < BOUNDS.x + BOUNDS.w - skin.select.padding_x
        ));
    }
}
