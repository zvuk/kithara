use num_traits::ToPrimitive;

use crate::{
    atoms::design::quad::{border, center_y},
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{FontFamily, SegmentedSkin, TextRoleSkin},
    text::TextContext,
};

/// A row of equal cells, one of them picked out.
pub(crate) struct Segmented {
    active_background: Rgba,
    active_text: Rgba,
    background: Rgba,
    frame: Rgba,
    inactive_text: Rgba,
    metrics: SegmentedSkin,
    role: TextRoleSkin,
}

/// What a segmented control is handed each frame: its words, and which of them
/// is the one picked.
pub(crate) struct SegmentedData {
    pub(crate) active: Option<usize>,
    pub(crate) items: Vec<String>,
}

impl Segmented {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.segmented;
        Self {
            active_background: skin.rgba(metrics.active_background),
            active_text: skin.rgba(metrics.active_text),
            background: skin.rgba(metrics.background),
            frame: skin.rgba(metrics.frame.border),
            inactive_text: skin.rgba(metrics.inactive_text),
            metrics,
            role: TextRoleSkin {
                color: metrics.inactive_text,
                font: FontFamily::Mono,
                size: metrics.text.size,
                spacing: 0.0,
                weight: metrics.text.weight,
            },
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        data: &SegmentedData,
        bounds: Rect,
    ) {
        let Some(width) = cell_width(bounds.w, data.items.len()) else {
            return;
        };
        list.fill_rect(bounds, self.background);
        if let Some(active) = data.active {
            list.fill_rect(Self::cell(bounds, active, width), self.active_background);
        }
        self.paint_grid(list, bounds, width, data.items.len());
        for (index, item) in data.items.iter().enumerate() {
            self.paint_word(
                list,
                text,
                item,
                Self::cell(bounds, index, width),
                index,
                data,
            );
        }
    }

    /// The box of one cell, counted from the left edge.
    fn cell(bounds: Rect, index: usize, width: f32) -> Rect {
        Rect {
            h: bounds.h,
            w: width,
            x: bounds.x + offset(index, width),
            y: bounds.y,
        }
    }

    /// The frame around the row and the hairlines between its cells.
    fn paint_grid(&self, list: &mut DrawListBuilder, bounds: Rect, width: f32, count: usize) {
        if self.metrics.frame.border_width <= 0.0 {
            return;
        }
        border(list, bounds, self.metrics.frame, self.frame);
        for index in 1..count {
            let x = bounds.x + offset(index, width);
            list.stroke_line(
                Pt { x, y: bounds.y },
                Pt {
                    x,
                    y: bounds.y + bounds.h,
                },
                self.frame,
                self.metrics.frame.border_width,
            );
        }
    }

    /// One cell's word, centred and clipped to the room the cell leaves it: a
    /// word longer than its cell is cut off rather than spilling into the next.
    fn paint_word(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        word: &str,
        cell: Rect,
        index: usize,
        data: &SegmentedData,
    ) {
        let color = if data.active == Some(index) {
            self.active_text
        } else {
            self.inactive_text
        };
        let run = text.shape(word, self.role, None);
        let mut word_list = DrawListBuilder::default();
        word_list.text(
            &run,
            word,
            Transform::translate(Pt {
                x: cell.x + (cell.w - run.width()) / 2.0,
                y: center_y(cell, &run),
            }),
            color,
        );
        list.clip(
            Rect {
                w: (cell.w - self.metrics.padding_x * 2.0).max(0.0),
                x: cell.x + self.metrics.padding_x,
                ..cell
            },
            word_list.finish(),
        );
    }
}

/// How wide one cell is, or nothing when there is no room or nothing to show.
pub(crate) fn cell_width(width: f32, count: usize) -> Option<f32> {
    let count = count.to_f32()?;
    (count > 0.0 && width > 0.0).then_some(width / count)
}

fn offset(index: usize, width: f32) -> f32 {
    index.to_f32().map_or(0.0, |index| index * width)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{DrawListBuilder, Rect, Segmented, SegmentedData, TextContext, cell_width};
    use crate::{builtin, draw::DrawList};

    const BOUNDS: Rect = Rect {
        h: 24.0,
        w: 180.0,
        x: 3.0,
        y: 5.0,
    };

    fn drawn(active: Option<usize>) -> DrawList {
        let skin = builtin::skin();
        let mut text = TextContext::from(skin.text_resources());
        let mut list = DrawListBuilder::default();
        Segmented::new(skin).paint(
            &mut list,
            &mut text,
            &SegmentedData {
                active,
                items: vec!["ONE".to_owned(), "TWO".to_owned(), "THREE".to_owned()],
            },
            BOUNDS,
        );
        list.finish()
    }

    /// The picked cell is what makes it a segmented control rather than a row
    /// of words, so it has to reach the picture.
    #[kithara::test]
    fn the_picked_cell_changes_the_picture() {
        assert_ne!(drawn(None), drawn(Some(1)));
    }

    #[kithara::test]
    fn a_different_cell_is_a_different_picture() {
        assert_ne!(drawn(Some(0)), drawn(Some(2)));
    }

    /// A row with nothing in it draws nothing rather than an empty frame.
    #[kithara::test]
    fn a_row_with_no_cells_draws_nothing() {
        assert_eq!(cell_width(BOUNDS.w, 0), None);
    }

    #[kithara::test]
    fn a_row_with_no_room_draws_nothing() {
        assert_eq!(cell_width(0.0, 3), None);
    }
}
