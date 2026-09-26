#[cfg(feature = "masonry")]
use crate::draw::{DrawListBuilder, Rgba};
use crate::{
    draw::{FillRule, Path, Pt, Rect, Verb},
    layout::{FrameCorners, FrameSides},
};

/// A box with `radius` taken off the corners `corners` names and the rest left
/// square.
///
/// The window has no outline of its own: what stands at its corner is a module,
/// and this is the shape that module fills to give the window one.
#[cfg(feature = "masonry")]
pub(crate) fn corner_path(bounds: Rect, radius: f32, corners: FrameCorners) -> Path {
    Path::new(FillRule::NonZero, corner_verbs(bounds, radius, corners))
}

/// The moves that draw [`corner_path`], so a ring can hold two of them.
fn corner_verbs(bounds: Rect, radius: f32, corners: FrameCorners) -> Vec<Verb> {
    /// How far a cubic control point sits along the tangent to draw a quarter
    /// circle. The classic circle-from-cubics constant.
    const BOW: f32 = 0.552_284_8;

    let radius = radius.min(bounds.w / 2.0).min(bounds.h / 2.0).max(0.0);
    let (left, right) = (bounds.x, bounds.x + bounds.w);
    let (top, bottom) = (bounds.y, bounds.y + bounds.h);
    let pick = |rounded: bool| if rounded { radius } else { 0.0 };
    let (tl, tr) = (pick(corners.top_left), pick(corners.top_right));
    let (br, bl) = (pick(corners.bottom_right), pick(corners.bottom_left));
    let arc = |from: Pt, corner: Pt, to: Pt| Verb::CurveTo {
        first: Pt {
            x: from.x + (corner.x - from.x) * BOW,
            y: from.y + (corner.y - from.y) * BOW,
        },
        second: Pt {
            x: to.x + (corner.x - to.x) * BOW,
            y: to.y + (corner.y - to.y) * BOW,
        },
        to,
    };
    let mut verbs: Vec<Verb> = Vec::with_capacity(9);
    verbs.push(Verb::MoveTo(Pt {
        x: left + tl,
        y: top,
    }));
    verbs.push(Verb::LineTo(Pt {
        x: right - tr,
        y: top,
    }));
    if tr > 0.0 {
        verbs.push(arc(
            Pt {
                x: right - tr,
                y: top,
            },
            Pt { x: right, y: top },
            Pt {
                x: right,
                y: top + tr,
            },
        ));
    }
    verbs.push(Verb::LineTo(Pt {
        x: right,
        y: bottom - br,
    }));
    if br > 0.0 {
        verbs.push(arc(
            Pt {
                x: right,
                y: bottom - br,
            },
            Pt {
                x: right,
                y: bottom,
            },
            Pt {
                x: right - br,
                y: bottom,
            },
        ));
    }
    verbs.push(Verb::LineTo(Pt {
        x: left + bl,
        y: bottom,
    }));
    if bl > 0.0 {
        verbs.push(arc(
            Pt {
                x: left + bl,
                y: bottom,
            },
            Pt { x: left, y: bottom },
            Pt {
                x: left,
                y: bottom - bl,
            },
        ));
    }
    verbs.push(Verb::LineTo(Pt {
        x: left,
        y: top + tl,
    }));
    if tl > 0.0 {
        verbs.push(arc(
            Pt {
                x: left,
                y: top + tl,
            },
            Pt { x: left, y: top },
            Pt {
                x: left + tl,
                y: top,
            },
        ));
    }
    verbs.push(Verb::Close);
    verbs
}

/// Fills the frame of a rounded box as a ring: the box's outline with the
/// outline of what it encloses cut out of it.
///
/// The frame is filled rather than stroked because that is what a square frame
/// already is - a band inside the box it frames - and a rounded corner must not
/// change where the band lies. A side the layout leaves out is trimmed by the
/// clip, which is exact: a side is only ever left out where the module meets
/// another one, and a corner there is square.
#[cfg(feature = "masonry")]
pub(crate) fn corner_frame(
    list: &mut DrawListBuilder,
    bounds: Rect,
    radius: f32,
    corners: FrameCorners,
    sides: FrameSides,
    color: Rgba,
    width: f32,
) {
    if width <= 0.0 {
        return;
    }
    let mut band = list.child();
    band.fill_path(corner_ring(bounds, radius, corners, width), color);
    list.clip(frame_clip(bounds, sides, width), band.finish());
}

/// The outline of a frame: the box's own, with the box it encloses cut out of
/// it, filled by the even-odd rule.
pub(crate) fn corner_ring(bounds: Rect, radius: f32, corners: FrameCorners, width: f32) -> Path {
    let inner = Rect {
        h: (bounds.h - width * 2.0).max(0.0),
        w: (bounds.w - width * 2.0).max(0.0),
        x: bounds.x + width,
        y: bounds.y + width,
    };
    let mut verbs = corner_verbs(bounds, radius, corners);
    verbs.extend(corner_verbs(inner, radius - width, corners));
    Path::new(FillRule::EvenOdd, verbs)
}

/// The part of `bounds` the frame is allowed to reach: every side the layout
/// draws, and nothing where it leaves one out.
pub(crate) fn frame_clip(bounds: Rect, sides: FrameSides, width: f32) -> Rect {
    let left = if sides.left { 0.0 } else { width };
    let right = if sides.right { 0.0 } else { width };
    let top = if sides.top { 0.0 } else { width };
    let bottom = if sides.bottom { 0.0 } else { width };
    Rect {
        h: (bounds.h - top - bottom).max(0.0),
        w: (bounds.w - left - right).max(0.0),
        x: bounds.x + left,
        y: bounds.y + top,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{FrameCorners, Rect, corner_verbs};
    use crate::draw::Verb;

    mod consts {
        use super::*;

        pub(super) const BOX: Rect = Rect {
            h: 40.0,
            w: 60.0,
            x: 10.0,
            y: 20.0,
        };
    }

    /// A box with no rounded corner is the rectangle it started as: four sides
    /// and no curve.
    #[kithara::test]
    fn a_box_with_no_rounded_corner_carries_no_curve() {
        let verbs = corner_verbs(consts::BOX, 8.0, FrameCorners::EMPTY);

        assert!(
            !verbs
                .iter()
                .any(|verb| matches!(verb, Verb::CurveTo { .. }))
        );
    }

    /// One named corner is one curve: the others stay square even though the
    /// radius reaches them.
    #[kithara::test]
    fn one_named_corner_is_one_curve() {
        let verbs = corner_verbs(
            consts::BOX,
            8.0,
            FrameCorners {
                top_left: true,
                ..FrameCorners::EMPTY
            },
        );

        assert_eq!(
            verbs
                .iter()
                .filter(|verb| matches!(verb, Verb::CurveTo { .. }))
                .count(),
            1
        );
    }

    /// A radius wider than the box it rounds is held to half of the box's
    /// shorter side, so the outline stays inside the box rather than crossing
    /// itself.
    #[kithara::test]
    fn a_radius_wider_than_the_box_is_held_to_half_of_its_shorter_side() {
        let verbs = corner_verbs(consts::BOX, 400.0, FrameCorners::ALL);
        let start = verbs.iter().find_map(|verb| match verb {
            Verb::MoveTo(point) => Some(*point),
            _ => None,
        });

        assert_eq!(
            start.map(|point| point.x),
            Some(consts::BOX.x + consts::BOX.h / 2.0)
        );
    }
}
