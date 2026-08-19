use crate::{
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Mark,
    shaping::TextContext,
};

/// One icon at one size: a lucide glyph shaped as text, or an authored outline
/// filled as a path.
///
/// Every control that shows an icon draws it the same way and differs only in
/// where it puts it, so the placement is the caller's and the drawing is here.
#[derive(Clone, Copy)]
pub(crate) struct Marked {
    mark: Mark,
    size: f32,
}

impl Marked {
    pub(crate) const fn new(mark: Mark, size: f32) -> Self {
        Self { mark, size }
    }

    /// How much room it takes across. A glyph is as wide as it was shaped; an
    /// outline is drawn in a square, so it is as wide as it is tall.
    pub(crate) fn width(&self, text: &mut TextContext) -> f32 {
        match self.mark {
            Mark::Glyph(ch) => text.shape_lucide(&ch.to_string(), self.size).width(),
            Mark::Outline(_) => self.size,
        }
    }

    /// Draws it with its left edge at `x`, centred down the box, and answers
    /// how wide it turned out.
    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        x: f32,
        bounds: Rect,
        color: Rgba,
    ) -> f32 {
        match self.mark {
            Mark::Glyph(ch) => {
                let mut encoded = [0_u8; 4];
                let content = ch.encode_utf8(&mut encoded);
                let run = text.shape_lucide(content, self.size);
                list.text(
                    &run,
                    content,
                    Transform::translate(Pt {
                        x,
                        y: bounds.y + (bounds.h - run.height()) / 2.0,
                    }),
                    color,
                );
                run.width()
            }
            Mark::Outline(outline) => {
                let path = outline.placed_with(
                    list,
                    Rect {
                        h: self.size,
                        w: self.size,
                        x,
                        y: bounds.y + (bounds.h - self.size) / 2.0,
                    },
                );
                list.fill_path(path, color);
                self.size
            }
        }
    }

    /// Draws it in the middle of the box, across as well as down.
    pub(crate) fn centred(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        bounds: Rect,
        color: Rgba,
    ) {
        let x = bounds.x + (bounds.w - self.width(text)) / 2.0;
        self.paint(list, text, x, bounds, color);
    }
}
