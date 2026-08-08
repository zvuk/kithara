#[cfg(feature = "render")]
use num_traits::ToPrimitive;

use super::{
    list::DrawList,
    path::Path,
    style::{Paint, Pen},
};
use crate::text::GlyphRun;

/// A toolkit-neutral RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub a: f32,
    pub b: f32,
    pub g: f32,
    pub r: f32,
}

/// A toolkit-neutral point in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

/// A toolkit-neutral rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub h: f32,
    pub w: f32,
    pub x: f32,
    pub y: f32,
}

impl Pt {
    #[cfg(feature = "render")]
    pub(crate) fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

impl Rect {
    #[cfg(feature = "render")]
    pub(crate) fn contains(self, point: Pt) -> bool {
        self.x <= point.x
            && point.x < self.x + self.w
            && self.y <= point.y
            && point.y < self.y + self.h
    }

    #[cfg(feature = "render")]
    pub(crate) fn uniform_horizontal_index(self, point: Pt, count: usize) -> Option<usize> {
        let last = count.checked_sub(1)?;
        let count = count.to_f32()?;
        let cell_width = (self.contains(point) && self.w > 0.0).then_some(self.w / count)?;
        ((point.x - self.x) / cell_width)
            .floor()
            .to_usize()
            .map(|index| index.min(last))
    }
}

/// A toolkit-neutral affine transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub xx: f32,
    pub xy: f32,
    pub yx: f32,
    pub yy: f32,
    pub dx: f32,
    pub dy: f32,
}

impl Transform {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        xx: 1.0,
        xy: 0.0,
        yx: 0.0,
        yy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    #[cfg(feature = "render")]
    pub(crate) const fn apply(self, point: Pt) -> Pt {
        Pt {
            x: self.xx * point.x + self.xy * point.y + self.dx,
            y: self.yx * point.x + self.yy * point.y + self.dy,
        }
    }

    /// Creates a translation transform.
    #[must_use]
    pub const fn translate(offset: Pt) -> Self {
        Self {
            dx: offset.x,
            dy: offset.y,
            ..Self::IDENTITY
        }
    }
}

/// Native geometry retained by a draw list.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Geom {
    /// A circular arc whose angles are expressed in radians.
    Arc {
        center: Pt,
        radius: f32,
        start: f32,
        end: f32,
    },
    Circle {
        center: Pt,
        radius: f32,
    },
    Line {
        from: Pt,
        to: Pt,
    },
    /// An outline no named shape covers.
    Path(Path),
    Rect(Rect),
    RoundedRect {
        rect: Rect,
        radius: f32,
    },
}

impl Geom {
    /// Whether this is an outline a backend has to be able to fill, rather than
    /// one of the shapes every backend names.
    #[must_use]
    pub const fn is_outline(&self) -> bool {
        matches!(self, Self::Path(_))
    }
}

/// A retained drawing command.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DrawCmd {
    /// A nested list scoped to a rectangular clip region.
    Clip {
        region: Rect,
        list: DrawList,
    },
    Fill {
        geom: Geom,
        paint: Paint,
    },
    Stroke {
        geom: Geom,
        color: Rgba,
        pen: Pen,
    },
    Text {
        run: GlyphRun,
        content: String,
        transform: Transform,
        color: Rgba,
    },
}
