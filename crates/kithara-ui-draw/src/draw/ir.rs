use kithara_ui_shaping::GlyphRun;

use super::{
    image::Image,
    list::DrawList,
    path::Path,
    pool::PoolText,
    style::{Paint, Pen},
};
use crate::geom::{Pt, Rect, Transform};

/// A toolkit-neutral RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "iced", derive(kithara_derive::Mirror))]
#[cfg_attr(feature = "iced", mirror(from = iced::Color, into = iced::Color))]
pub struct Rgba {
    pub a: f32,
    pub b: f32,
    pub g: f32,
    pub r: f32,
}

/// Paints nothing: what a control draws where its skin names no colour.
pub const TRANSPARENT: Rgba = Rgba {
    a: 0.0,
    b: 0.0,
    g: 0.0,
    r: 0.0,
};

/// Native geometry retained by a draw list.
#[derive(Clone, Debug, PartialEq)]
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
