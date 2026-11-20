use crate::{
    atoms::design::corner::{corner_ring, corner_verbs, frame_clip},
    draw::{DrawListBuilder, FillRule, Path, Rect, Rgba},
    layout::{FrameCorners, FrameSides},
};

/// A box with `radius` taken off the corners `corners` names and the rest left
/// square.
///
/// The window has no outline of its own: what stands at its corner is a module,
/// and this is the shape that module fills to give the window one.
pub(crate) fn corner_path(bounds: Rect, radius: f32, corners: FrameCorners) -> Path {
    Path::new(FillRule::NonZero, corner_verbs(bounds, radius, corners))
}

/// Fills the frame of a rounded box as a ring: the box's outline with the
/// outline of what it encloses cut out of it.
///
/// The frame is filled rather than stroked because that is what a square frame
/// already is - a band inside the box it frames - and a rounded corner must not
/// change where the band lies. A side the layout leaves out is trimmed by the
/// clip, which is exact: a side is only ever left out where the module meets
/// another one, and a corner there is square.
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

pub(crate) fn paint_node_background(
    bounds: Rect,
    color: Option<Rgba>,
    round: FrameCorners,
    radius: f32,
    list: &mut DrawListBuilder,
) {
    let Some(color) = color else {
        return;
    };
    if round.any() && radius > 0.0 {
        list.fill_path(corner_path(bounds, radius, round), color);
    } else {
        list.fill_rect(bounds, color);
    }
}

pub(crate) fn paint_node_frame(
    bounds: Rect,
    frame: Option<(FrameSides, Rgba, f32)>,
    round: FrameCorners,
    radius: f32,
    list: &mut DrawListBuilder,
) {
    let Some((sides, color, width)) = frame else {
        return;
    };
    if round.any() && radius > 0.0 {
        corner_frame(list, bounds, radius, round, sides, color, width);
        return;
    }
    let right = bounds.x + (bounds.w - width).max(0.0);
    let bottom = bounds.y + (bounds.h - width).max(0.0);
    let mut side = |x: f32, y: f32, w: f32, h: f32| {
        list.fill_rect(
            Rect {
                x,
                y,
                w: w.max(0.0),
                h: h.max(0.0),
            },
            color,
        );
    };
    if sides.top {
        side(bounds.x, bounds.y, bounds.w, width);
    }
    if sides.right {
        side(right, bounds.y, width, bounds.h);
    }
    if sides.bottom {
        side(bounds.x, bottom, bounds.w, width);
    }
    if sides.left {
        side(bounds.x, bounds.y, width, bounds.h);
    }
}
