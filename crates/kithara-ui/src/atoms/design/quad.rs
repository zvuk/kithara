use crate::{
    draw::{DrawListBuilder, Rect, Rgba},
    shaping::GlyphRun,
    skin::FrameSkin,
};

/// The filled, framed box almost every control is built on.
pub(crate) fn quad(
    list: &mut DrawListBuilder,
    bounds: Rect,
    frame: FrameSkin,
    fill: Rgba,
    stroke: Rgba,
) {
    list.fill_rounded_rect(bounds, frame.radius, fill);
    border(list, bounds, frame, stroke);
}

/// A frame drawn inside `bounds` rather than astride its edge, so a bordered
/// control occupies exactly the box it was given.
pub(crate) fn border(list: &mut DrawListBuilder, bounds: Rect, frame: FrameSkin, color: Rgba) {
    if frame.border_width <= 0.0 {
        return;
    }
    let inset = frame.border_width / 2.0;
    list.stroke_rounded_rect(
        Rect {
            h: (bounds.h - frame.border_width).max(0.0),
            w: (bounds.w - frame.border_width).max(0.0),
            x: bounds.x + inset,
            y: bounds.y + inset,
        },
        frame.radius,
        color,
        frame.border_width,
    );
}

/// Where a run sits when it is centred down the box but placed across it by
/// the caller.
pub(crate) fn center_y(bounds: Rect, run: &GlyphRun) -> f32 {
    bounds.y + (bounds.h - run.height()) / 2.0
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{Rect, border, quad};
    use crate::{
        draw::{DrawCmd, DrawListBuilder, Geom, Pen, Rgba},
        skin::{ColorRole, FrameSkin},
    };

    /// The box both checks frame, and the colour they frame it in.
    const BOX: (Rect, Rgba) = (
        Rect {
            h: 20.0,
            w: 40.0,
            x: 4.0,
            y: 6.0,
        },
        Rgba {
            a: 1.0,
            b: 1.0,
            g: 1.0,
            r: 1.0,
        },
    );

    const fn framed(border_width: f32) -> FrameSkin {
        FrameSkin {
            border: ColorRole::Line,
            border_width,
            radius: 0.0,
        }
    }

    /// A frame drawn on the edge of the box bleeds half its width outside, which
    /// makes neighbouring controls overlap. It has to be inset instead.
    #[kithara::test]
    fn a_border_sits_inside_the_box_it_frames() {
        let mut list = DrawListBuilder::default();
        border(&mut list, BOX.0, framed(2.0), BOX.1);

        assert!(matches!(
            list.finish().commands(),
            [DrawCmd::Stroke {
                geom: Geom::Rect(Rect {
                    h: 18.0,
                    w: 38.0,
                    x: 5.0,
                    y: 7.0,
                }),
                pen: Pen { width: 2.0, .. },
                ..
            }]
        ));
    }

    /// A skin can ask for no frame at all, and then the control must not draw a
    /// hairline nobody asked for.
    #[kithara::test]
    fn a_quad_without_a_border_draws_only_its_fill() {
        let mut list = DrawListBuilder::default();
        quad(&mut list, BOX.0, framed(0.0), BOX.1, BOX.1);

        assert!(matches!(list.finish().commands(), [DrawCmd::Fill { .. }]));
    }
}
