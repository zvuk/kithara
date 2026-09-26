#[cfg(feature = "shape")]
mod shape;
mod style;

#[cfg(feature = "shape")]
pub use shape::{
    FontId, FontPolicy, Glyph, GlyphFace, GlyphRun, GlyphSegment, TextContext, TextError,
    TextResources,
};
pub use style::{FontFamily, FontWeight, TextStyle};
