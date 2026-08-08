use crate::{
    atoms::design::quad::center_y,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{FontFamily, GlobalBarSkin, TextRoleSkin},
    text::TextContext,
};

/// The wordmark at the head of the global bar, one letter at a time.
///
/// The letters are spaced by hand rather than shaped as a word: the gap
/// between them is what makes it a wordmark instead of a caption.
const LETTERS: [&str; 7] = ["K", "I", "T", "H", "A", "R", "A"];

pub(crate) struct Brand {
    metrics: GlobalBarSkin,
    panel: Rgba,
    role: TextRoleSkin,
    text: Rgba,
}

impl Brand {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.global_bar;
        Self {
            metrics,
            panel: skin.palette.bg_panel,
            role: TextRoleSkin {
                color: crate::skin::ColorRole::Text,
                font: FontFamily::Display,
                size: metrics.brand_text.size,
                spacing: 0.0,
                weight: metrics.brand_text.weight,
            },
            text: skin.palette.text,
        }
    }

    pub(crate) fn paint(&self, list: &mut DrawListBuilder, text: &mut TextContext, bounds: Rect) {
        list.fill_rect(bounds, self.panel);
        let mut x = bounds.x + self.metrics.brand_padding_x;
        for letter in LETTERS {
            let run = text.shape(letter, self.role, None);
            list.text(
                &run,
                letter,
                Transform::translate(Pt {
                    x,
                    y: center_y(bounds, &run),
                }),
                self.text,
            );
            x += run.width() + self.metrics.brand_gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Brand, DrawListBuilder, LETTERS, Rect, TextContext};
    use crate::{builtin, draw::DrawCmd};

    /// Every letter is drawn, in order, each one clear of the last.
    #[kithara::test]
    fn the_wordmark_spells_itself_left_to_right() {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Brand::new(skin).paint(
            &mut list,
            &mut text,
            Rect {
                h: 32.0,
                w: 120.0,
                x: 4.0,
                y: 2.0,
            },
        );
        let list = list.finish();

        let letters: Vec<_> = list
            .commands()
            .iter()
            .filter_map(|command| match command {
                DrawCmd::Text {
                    content, transform, ..
                } => Some((content.clone(), transform.dx)),
                _ => None,
            })
            .collect();

        assert_eq!(
            letters
                .iter()
                .map(|(word, _)| word.as_str())
                .collect::<Vec<_>>(),
            LETTERS
        );
        assert!(letters.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }
}
