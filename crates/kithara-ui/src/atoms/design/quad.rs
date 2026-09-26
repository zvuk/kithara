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

/// The whole pixel a coordinate belongs to, ties going the same way on both
/// sides of the origin.
pub(crate) fn snap(value: f32) -> f32 {
    (value + 0.5).floor()
}

/// A rule down `bounds`, drawn on the pixel columns it covers.
///
/// A stroked line is centred on its own coordinate, so a rule one pixel wide
/// lands half on each side of it. A backend that samples area then draws two
/// dim columns while one that snaps hairlines draws a single crisp one, which
/// is two hosts drawing the same mark differently - and neither is the mark the
/// skin asked for. Filling the columns the rule covers puts the same pixels on
/// both.
pub(crate) fn rule(list: &mut DrawListBuilder, bounds: Rect, x: f32, width: f32, color: Rgba) {
    let left = snap(x - width / 2.0);
    list.fill_rect(
        Rect {
            h: bounds.h,
            w: (snap(x + width / 2.0) - left).max(1.0),
            x: left,
            y: bounds.y,
        },
        color,
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

    use super::{Rect, border, quad, rule};
    use crate::{
        draw::{DrawCmd, DrawListBuilder, Geom, Pen, Rgba},
        skin::{ColorRole, FrameSkin},
    };

    /// The boxes and the ink the checks below are drawn with.
    mod consts {
        use super::{Rect, Rgba};

        /// The box both frame checks draw into.
        pub(super) const BOX: Rect = Rect {
            h: 20.0,
            w: 40.0,
            x: 4.0,
            y: 6.0,
        };

        /// The box every rule below runs down.
        pub(super) const COLUMN: Rect = Rect {
            h: 60.0,
            w: 300.0,
            x: 0.0,
            y: 12.0,
        };

        /// The colour every mark is asked for in.
        pub(super) const INK: Rgba = Rgba {
            a: 1.0,
            b: 1.0,
            g: 1.0,
            r: 1.0,
        };
    }

    const fn framed(border_width: f32) -> FrameSkin {
        FrameSkin {
            border_width,
            border: ColorRole::Line,
            radius: 0.0,
        }
    }

    /// A frame drawn on the edge of the box bleeds half its width outside, which
    /// makes neighbouring controls overlap. It has to be inset instead.
    #[kithara::test]
    fn a_border_sits_inside_the_box_it_frames() {
        let mut list = DrawListBuilder::default();
        border(&mut list, consts::BOX, framed(2.0), consts::INK);

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
        quad(
            &mut list,
            consts::BOX,
            framed(0.0),
            consts::INK,
            consts::INK,
        );

        assert!(matches!(list.finish().commands(), [DrawCmd::Fill { .. }]));
    }

    fn ruled(x: f32, width: f32) -> Rect {
        let mut list = DrawListBuilder::default();
        rule(&mut list, consts::COLUMN, x, width, consts::INK);
        let commands = list.finish().commands().to_vec();
        match commands.as_slice() {
            [
                DrawCmd::Fill {
                    geom: Geom::Rect(rect),
                    ..
                },
            ] => *rect,
            other => panic!("a rule must fill one rectangle: {other:?}"),
        }
    }

    /// A hairline centred on its own coordinate lands half on each side of it,
    /// which the two rasterisers resolve differently. It has to cover a column.
    #[kithara::test]
    fn a_rule_between_two_columns_covers_the_one_it_falls_in() {
        let drawn = ruled(10.3, 1.0);

        assert_eq!((drawn.x, drawn.w), (10.0, 1.0));
    }

    #[kithara::test]
    fn a_rule_on_a_column_edge_covers_one_column_too() {
        let drawn = ruled(10.0, 1.0);

        assert_eq!((drawn.x, drawn.w), (10.0, 1.0));
    }

    #[kithara::test]
    fn a_rule_keeps_the_width_the_skin_gives_it() {
        assert_eq!(ruled(10.0, 2.0).w, 2.0);
    }

    #[kithara::test]
    fn a_rule_runs_the_height_of_the_box_it_marks() {
        let drawn = ruled(10.3, 1.0);

        assert_eq!((drawn.y, drawn.h), (consts::COLUMN.y, consts::COLUMN.h));
    }
}
