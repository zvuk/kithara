//! Geometry under a transform.
//!
//! A named shape survives a transform only while the transform leaves it
//! nameable: a rectangle stays a rectangle while the axes stay where they are,
//! a circle stays a circle only under a rotation and one scale. Everything else
//! becomes an outline here, in the neutral list, so that both hosts replay the
//! same points instead of each asking its own toolkit to turn a rectangle.

use kurbo::{Arc, Circle, PathEl, Point, RoundedRect, Shape};
use num_traits::cast::AsPrimitive;

use super::{
    ir::{DrawCmd, Geom, Pt, Rect, Transform},
    list::DrawList,
    path::{Path, Verb},
};

/// How closely a flattened curve follows the one it replaces, in logical
/// pixels. A tenth of a pixel is under what either rasteriser resolves.
const TOLERANCE: f64 = 0.1;

/// The upright rectangle that holds this one after the transform.
///
/// Exact while the transform keeps the axes — which is the only case a caller
/// is allowed to keep a `Rect` for. Where a rectangle must stay a rectangle
/// even though the transform turned it, this is the box it fits in: a clip
/// region and an image destination have nowhere else to go, because neither
/// the neutral command nor either toolkit's image carries a matrix.
pub(super) fn bounds(rect: Rect, by: Transform) -> Rect {
    let corners = [
        by.apply(Pt {
            x: rect.x,
            y: rect.y,
        }),
        by.apply(Pt {
            x: rect.x + rect.w,
            y: rect.y,
        }),
        by.apply(Pt {
            x: rect.x + rect.w,
            y: rect.y + rect.h,
        }),
        by.apply(Pt {
            x: rect.x,
            y: rect.y + rect.h,
        }),
    ];
    let fold = |pick: fn(f32, f32) -> f32, of: fn(Pt) -> f32| {
        corners
            .iter()
            .map(|corner| of(*corner))
            .fold(of(corners[0]), pick)
    };
    let (left, right) = (fold(f32::min, |at| at.x), fold(f32::max, |at| at.x));
    let (top, bottom) = (fold(f32::min, |at| at.y), fold(f32::max, |at| at.y));
    Rect {
        h: bottom - top,
        w: right - left,
        x: left,
        y: top,
    }
}

/// The box a picture lands in, and how far it is turned inside it.
///
/// A picture is not flattened to an outline the way a shape is: both
/// rasterisers draw it into a rectangle and turn it about that rectangle's
/// centre, so that is the pair this reads off the transform. Exact for a
/// transform built from a move, a turn and a scale, which is every pose a
/// document can write.
pub(super) fn turned(rect: Rect, by: Transform) -> (Rect, f32) {
    let centre = by.apply(Pt {
        x: rect.x + rect.w / 2.0,
        y: rect.y + rect.h / 2.0,
    });
    let across = by.xx.hypot(by.yx);
    let down = by.xy.hypot(by.yy);
    let (w, h) = (rect.w * across, rect.h * down);
    (
        Rect {
            h,
            w,
            x: centre.x - w / 2.0,
            y: centre.y - h / 2.0,
        },
        by.yx.atan2(by.xx),
    )
}

/// The upright rectangle a turned picture reaches into.
pub(super) fn turned_ink(rect: Rect, turn: f32) -> Rect {
    if turn == 0.0 {
        return rect;
    }
    let centre = Pt {
        x: rect.x + rect.w / 2.0,
        y: rect.y + rect.h / 2.0,
    };
    let about = Transform::translate(Pt {
        x: -centre.x,
        y: -centre.y,
    })
    .then(Transform::rotate(turn))
    .then(Transform::translate(centre));
    bounds(rect, about)
}

/// The upright rectangle every command in `list` draws inside, if it draws.
///
/// A painter is asked to draw inside the box it was handed, so this is the
/// answer for a control nobody moved. It stops being the answer the moment a
/// pose carries the drawing somewhere else, which is the one case a caller
/// needs it for: a host that clipped to the box would drop what left it.
pub(crate) fn ink(list: &DrawList) -> Option<Rect> {
    list.commands().iter().filter_map(command_ink).reduce(union)
}

fn command_ink(command: &DrawCmd) -> Option<Rect> {
    match command {
        DrawCmd::Clip { region, .. } => Some(*region),
        DrawCmd::Fill { geom, .. } => geom_ink(geom),
        DrawCmd::Image { rect, turn, .. } => Some(turned_ink(*rect, *turn)),
        DrawCmd::Stroke { geom, pen, .. } => {
            geom_ink(geom).map(|geom| grown(geom, pen.width / 2.0))
        }
        // The run's own box, which the transform then carries wherever the text
        // went. Ascenders and descenders past the measured height are covered by
        // taking the height on both sides of the baseline.
        DrawCmd::Text { run, transform, .. } => Some(bounds(
            Rect {
                h: run.height() * 2.0,
                w: run.width(),
                x: 0.0,
                y: -run.height(),
            },
            *transform,
        )),
    }
}

fn geom_ink(geom: &Geom) -> Option<Rect> {
    let rect = match geom {
        Geom::Arc { center, radius, .. } | Geom::Circle { center, radius } => Rect {
            h: radius * 2.0,
            w: radius * 2.0,
            x: center.x - radius,
            y: center.y - radius,
        },
        Geom::Line { from, to } => span(*from, *to),
        Geom::Path(path) => return path_ink(path),
        Geom::Rect(rect) | Geom::RoundedRect { rect, .. } => *rect,
    };
    Some(rect)
}

/// An outline covers the points it names, and a path that names none — which a
/// painter emits for an icon it has no art for — covers nothing rather than
/// everything.
fn path_ink(path: &Path) -> Option<Rect> {
    path.verbs()
        .iter()
        .flat_map(|verb| points(*verb))
        .flatten()
        .map(|at| span(at, at))
        .reduce(union)
}

/// The points a verb names, in the order it draws them.
fn points(verb: Verb) -> [Option<Pt>; 3] {
    match verb {
        Verb::Close => [None, None, None],
        Verb::CurveTo { first, second, to } => [Some(first), Some(second), Some(to)],
        Verb::LineTo(to) | Verb::MoveTo(to) => [Some(to), None, None],
        Verb::QuadTo { control, to } => [Some(control), Some(to), None],
    }
}

fn span(first: Pt, second: Pt) -> Rect {
    Rect {
        h: (second.y - first.y).abs(),
        w: (second.x - first.x).abs(),
        x: first.x.min(second.x),
        y: first.y.min(second.y),
    }
}

fn grown(rect: Rect, by: f32) -> Rect {
    Rect {
        h: rect.h + by * 2.0,
        w: rect.w + by * 2.0,
        x: rect.x - by,
        y: rect.y - by,
    }
}

pub(crate) fn union(first: Rect, second: Rect) -> Rect {
    let left = first.x.min(second.x);
    let top = first.y.min(second.y);
    let right = (first.x + first.w).max(second.x + second.w);
    let bottom = (first.y + first.h).max(second.y + second.h);
    Rect {
        h: bottom - top,
        w: right - left,
        x: left,
        y: top,
    }
}

pub(super) fn rect_verbs(shape: Rect, by: Transform) -> Vec<Verb> {
    let corner = |x: f32, y: f32| Verb::LineTo(by.apply(Pt { x, y }));
    vec![
        Verb::MoveTo(by.apply(Pt {
            x: shape.x,
            y: shape.y,
        })),
        corner(shape.x + shape.w, shape.y),
        corner(shape.x + shape.w, shape.y + shape.h),
        corner(shape.x, shape.y + shape.h),
        Verb::Close,
    ]
}

pub(super) fn rounded_verbs(shape: Rect, radius: f32, by: Transform) -> Vec<Verb> {
    let radius: f64 = radius.as_();
    flatten(
        &RoundedRect::new(
            shape.x.as_(),
            shape.y.as_(),
            (shape.x + shape.w).as_(),
            (shape.y + shape.h).as_(),
            radius,
        ),
        by,
    )
}

pub(super) fn circle_verbs(center: Pt, radius: f32, by: Transform) -> Vec<Verb> {
    flatten(
        &Circle::new(Point::new(center.x.as_(), center.y.as_()), radius.as_()),
        by,
    )
}

pub(super) fn arc_verbs(center: Pt, radius: f32, start: f32, end: f32, by: Transform) -> Vec<Verb> {
    let radius: f64 = radius.as_();
    let start: f64 = start.as_();
    let end: f64 = end.as_();
    flatten(
        &Arc::new(
            Point::new(center.x.as_(), center.y.as_()),
            (radius, radius),
            start,
            end - start,
            0.0,
        ),
        by,
    )
}

pub(super) fn path_verbs<'a, Verbs>(verbs: Verbs, by: Transform) -> Vec<Verb>
where
    Verbs: IntoIterator<Item = &'a Verb>,
{
    verbs.into_iter().map(|verb| moved(*verb, by)).collect()
}

fn moved(verb: Verb, by: Transform) -> Verb {
    match verb {
        Verb::Close => Verb::Close,
        Verb::CurveTo { first, second, to } => Verb::CurveTo {
            first: by.apply(first),
            second: by.apply(second),
            to: by.apply(to),
        },
        Verb::LineTo(to) => Verb::LineTo(by.apply(to)),
        Verb::MoveTo(to) => Verb::MoveTo(by.apply(to)),
        Verb::QuadTo { control, to } => Verb::QuadTo {
            control: by.apply(control),
            to: by.apply(to),
        },
    }
}

fn flatten(shape: &impl Shape, by: Transform) -> Vec<Verb> {
    shape
        .path_elements(TOLERANCE)
        .map(|element| element_verb(element, by))
        .collect()
}

fn element_verb(element: PathEl, by: Transform) -> Verb {
    let at = |point: Point| {
        by.apply(Pt {
            x: point.x.as_(),
            y: point.y.as_(),
        })
    };
    match element {
        PathEl::ClosePath => Verb::Close,
        PathEl::CurveTo(first, second, to) => Verb::CurveTo {
            first: at(first),
            second: at(second),
            to: at(to),
        },
        PathEl::LineTo(to) => Verb::LineTo(at(to)),
        PathEl::MoveTo(to) => Verb::MoveTo(at(to)),
        PathEl::QuadTo(control, to) => Verb::QuadTo {
            control: at(control),
            to: at(to),
        },
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{Transform, bounds, ink, rect_verbs, turned, turned_ink};
    use crate::draw::{DrawListBuilder, Image, ImageId, Pt, Rect, Rgba, Verb};

    const BOX: Rect = Rect {
        h: 20.0,
        w: 40.0,
        x: 10.0,
        y: 5.0,
    };

    #[kithara::test]
    fn a_translation_moves_a_rectangle_and_keeps_its_extent() {
        let moved = bounds(BOX, Transform::translate(Pt { x: 3.0, y: -2.0 }));

        assert_eq!(
            moved,
            Rect {
                h: 20.0,
                w: 40.0,
                x: 13.0,
                y: 3.0,
            }
        );
    }

    /// A mirror is a scale by a negative factor, and a rectangle has no
    /// negative extent to hand a backend.
    #[kithara::test]
    fn a_mirrored_rectangle_is_normalised() {
        let mirrored = bounds(BOX, Transform::scale(Pt { x: -1.0, y: 1.0 }));

        assert_eq!(mirrored.x, -50.0);
    }

    #[kithara::test]
    fn a_mirrored_rectangle_keeps_its_width() {
        let mirrored = bounds(BOX, Transform::scale(Pt { x: -1.0, y: 1.0 }));

        assert_eq!(mirrored.w, 40.0);
    }

    #[kithara::test]
    fn a_rectangle_walked_as_an_outline_closes() {
        let verbs = rect_verbs(BOX, Transform::IDENTITY);

        assert_eq!(verbs.last(), Some(&Verb::Close));
    }

    /// A quarter turn sends the near corner `(10, 5)` to `(-5, 10)`. A quarter
    /// turn has no exact `sin_cos` in `f32`, so the corner is read at the pixel
    /// it falls in rather than at its bits; the property being checked is where
    /// the rotation puts it, not how the library rounds.
    #[kithara::test]
    fn a_quarter_turn_puts_the_first_corner_where_the_rotation_sends_it() {
        let verbs = rect_verbs(BOX, Transform::rotate(core::f32::consts::FRAC_PI_2));
        let Some(&Verb::MoveTo(corner)) = verbs.first() else {
            panic!("a rectangle starts by moving to its near corner");
        };

        assert_eq!(
            Pt {
                x: corner.x.round(),
                y: corner.y.round(),
            },
            Pt { x: -5.0, y: 10.0 }
        );
    }

    #[kithara::test]
    fn a_list_that_draws_nothing_inks_nothing() {
        assert_eq!(ink(&DrawListBuilder::default().finish()), None);
    }

    #[kithara::test]
    fn a_filled_rectangle_inks_itself() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(BOX, paint());

        assert_eq!(ink(&list.finish()), Some(BOX));
    }

    /// Half a pen sits either side of the line it draws, so a stroked shape
    /// covers more than the shape does.
    #[kithara::test]
    fn a_stroke_inks_half_its_pen_past_the_shape() {
        let mut list = DrawListBuilder::default();
        list.stroke_rounded_rect(BOX, 0.0, paint(), 4.0);

        let Some(inked) = ink(&list.finish()) else {
            panic!("a stroked rectangle inks something");
        };
        assert_eq!(inked.x, BOX.x - 2.0);
    }

    /// The one case the caller needs: a pose carried the drawing out of the box
    /// it was handed, and the answer has to follow it rather than stop at the
    /// box.
    #[kithara::test]
    fn a_transformed_list_inks_where_the_transform_put_it() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::translate(Pt { x: 100.0, y: 0.0 }), |list| {
            list.fill_rect(BOX, paint());
        });

        let Some(inked) = ink(&list.finish()) else {
            panic!("a moved rectangle inks something");
        };
        assert_eq!(inked.x, BOX.x + 100.0);
    }

    #[kithara::test]
    fn a_list_of_two_shapes_inks_both() {
        let mut list = DrawListBuilder::default();
        list.fill_rect(BOX, paint());
        list.fill_circle(Pt { x: 0.0, y: 0.0 }, 5.0, paint());

        let Some(inked) = ink(&list.finish()) else {
            panic!("two shapes ink something");
        };
        assert_eq!(inked.x, -5.0);
    }

    #[kithara::test]
    fn a_turned_picture_keeps_the_size_it_was_given() {
        let (box_of, _) = turned(BOX, Transform::rotate(std::f32::consts::FRAC_PI_4));

        assert!((box_of.w - BOX.w).abs() < 1e-4);
    }

    #[kithara::test]
    fn a_turned_picture_reports_the_angle_it_was_turned_by() {
        let (_, turn) = turned(BOX, Transform::rotate(std::f32::consts::FRAC_PI_4));

        assert!((turn - std::f32::consts::FRAC_PI_4).abs() < 1e-4);
    }

    #[kithara::test]
    fn a_scaled_picture_reports_the_size_the_scale_gave_it() {
        let (box_of, _) = turned(BOX, Transform::scale(Pt { x: 2.0, y: 3.0 }));

        assert_eq!([box_of.w, box_of.h], [BOX.w * 2.0, BOX.h * 3.0]);
    }

    #[kithara::test]
    fn a_moved_picture_lands_where_the_move_put_its_centre() {
        let (box_of, _) = turned(BOX, Transform::translate(Pt { x: 7.0, y: -3.0 }));

        assert_eq!([box_of.x, box_of.y], [BOX.x + 7.0, BOX.y - 3.0]);
    }

    /// A picture is drawn turned rather than flattened, so what it reaches is
    /// wider than the box it was given — which is what a host must not clip to.
    #[kithara::test]
    fn a_turned_picture_reaches_outside_its_own_box() {
        let reached = turned_ink(BOX, std::f32::consts::FRAC_PI_4);

        assert!(reached.w > BOX.w);
    }

    #[kithara::test]
    fn an_upright_picture_reaches_exactly_its_own_box() {
        assert_eq!(turned_ink(BOX, 0.0), BOX);
    }

    #[kithara::test]
    fn a_picture_under_a_pose_inks_where_the_pose_carried_it() {
        let mut list = DrawListBuilder::default();
        list.transformed(Transform::translate(Pt { x: 100.0, y: 0.0 }), |list| {
            list.image(picture(), BOX);
        });

        let Some(inked) = ink(&list.finish()) else {
            panic!("a picture inks something");
        };
        assert_eq!(inked.x, BOX.x + 100.0);
    }

    fn picture() -> Image {
        Image::pixels(ImageId::new("test/sprite"), 1, 1, Arc::from(vec![0_u8; 4]))
            .unwrap_or_else(|| panic!("one opaque pixel is a picture"))
    }

    /// One opaque colour, because these tests are about where ink lands and
    /// never about what colour it is.
    const fn paint() -> Rgba {
        Rgba {
            a: 1.0,
            b: 1.0,
            g: 1.0,
            r: 1.0,
        }
    }
}
