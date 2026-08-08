use crate::{
    atoms::design::quad::quad,
    draw::{DrawListBuilder, Pt, Rect, Rgba, Transform},
    render::Skin,
    skin::{CellSkin, FrameSkin, TextRoleSkin},
    text::TextContext,
};

/// A framed box with a caption under it.
pub(crate) struct Cell {
    background: Rgba,
    highlighted: Face,
    idle: Face,
    metrics: CellSkin,
    role: TextRoleSkin,
}

/// How the cell looks in one of its two states, resolved from the skin when the
/// cell is built.
struct Face {
    border: Rgba,
    frame: FrameSkin,
    label: Rgba,
}

impl Cell {
    pub(crate) fn new(skin: &Skin) -> Self {
        let metrics = skin.cell;
        Self {
            background: skin.rgba(metrics.background),
            highlighted: Face {
                border: skin.rgba(metrics.highlighted_frame.border),
                frame: metrics.highlighted_frame,
                label: skin.rgba(metrics.highlighted_frame.border),
            },
            idle: Face {
                border: skin.rgba(metrics.frame.border),
                frame: metrics.frame,
                label: skin.rgba(skin.text.micro_label.color),
            },
            metrics,
            role: skin.text.micro_label,
        }
    }

    pub(crate) fn paint(
        &self,
        list: &mut DrawListBuilder,
        text: &mut TextContext,
        label: Option<&str>,
        highlighted: bool,
        bounds: Rect,
    ) {
        let face = if highlighted {
            &self.highlighted
        } else {
            &self.idle
        };
        let band = label.map_or(0.0, |_| self.metrics.label_height);
        let gap = label.map_or(0.0, |_| self.metrics.label_gap);
        let box_bounds = Rect {
            h: (bounds.h - gap - band).max(0.0),
            ..bounds
        };
        quad(list, box_bounds, face.frame, self.background, face.border);

        let Some(label) = label else {
            return;
        };
        let content = label.to_uppercase();
        let run = text.shape(&content, self.role, Some(bounds.w));
        list.text(
            &run,
            &content,
            Transform::translate(Pt {
                x: bounds.x + (bounds.w - run.width()) / 2.0,
                y: bounds.y + box_bounds.h + gap,
            }),
            face.label,
        );
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Cell, DrawListBuilder, Rect, TextContext};
    use crate::{
        builtin,
        draw::{DrawCmd, Geom},
    };

    /// The caption sits in a band below the box, so the box has to give up
    /// exactly that band plus the gap — otherwise the two overlap.
    #[kithara::test]
    fn the_box_gives_up_the_band_its_caption_needs() {
        let skin = builtin::skin();
        let bounds = Rect {
            h: 36.0,
            w: 40.0,
            x: 2.0,
            y: 3.0,
        };
        let cell = Cell::new(skin);
        let draw = |label, highlighted| {
            let mut text = TextContext::from(skin.text_resources());
            let mut list = DrawListBuilder::default();
            cell.paint(&mut list, &mut text, label, highlighted, bounds);
            list.finish()
        };

        let captioned = draw(Some("a1"), false);
        let [
            DrawCmd::Fill {
                geom: Geom::Rect(box_rect),
                ..
            },
            _,
            DrawCmd::Text {
                content, transform, ..
            },
        ] = captioned.commands()
        else {
            panic!("a captioned cell must draw its box, its frame, then its caption");
        };
        assert_eq!(
            box_rect.h,
            bounds.h - skin.cell.label_gap - skin.cell.label_height
        );
        assert_eq!(
            content, "A1",
            "the caption is upper-cased like the iced one"
        );
        assert_eq!(
            transform.dy,
            bounds.y + box_rect.h + skin.cell.label_gap,
            "the caption starts where the box ends"
        );

        let bare = draw(None, false);
        let [
            DrawCmd::Fill {
                geom: Geom::Rect(bare_box),
                ..
            },
            ..,
        ] = bare.commands()
        else {
            panic!("a bare cell must still draw its box");
        };
        assert_eq!(
            bare_box.h, bounds.h,
            "a cell with no caption keeps the whole box"
        );

        assert_ne!(
            draw(Some("a1"), true),
            captioned,
            "highlighting must change what the cell draws"
        );
    }
}
