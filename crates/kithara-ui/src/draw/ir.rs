#[cfg(feature = "render")]
use num_traits::ToPrimitive;

use super::{
    image::Image,
    list::DrawList,
    path::Path,
    style::{Paint, Pen},
    text::PoolText,
};
pub use crate::geom::{Pt, Transform};
use crate::shaping::GlyphRun;

/// A toolkit-neutral RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub a: f32,
    pub b: f32,
    pub g: f32,
    pub r: f32,
}

/// A toolkit-neutral rectangle in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub h: f32,
    pub w: f32,
    pub x: f32,
    pub y: f32,
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
    /// Draws one picture into `rect`, turned by `turn` radians about the
    /// rectangle's own centre.
    ///
    /// A box and a turn, rather than a matrix: that is what both rasterisers
    /// take for a picture, and it is exactly enough for one that is moved,
    /// resized and rotated.
    Image {
        image: Image,
        rect: Rect,
        turn: f32,
    },
    Stroke {
        geom: Geom,
        color: Rgba,
        pen: Pen,
    },
    Text {
        run: GlyphRun,
        content: PoolText,
        transform: Transform,
        color: Rgba,
    },
}
