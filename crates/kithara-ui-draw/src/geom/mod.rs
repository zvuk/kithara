//! Toolkit-neutral geometry.
//!
//! A point, a rectangle and an affine transform are not drawing commands:
//! expansion folds a document's poses into a transform long before anything is
//! painted, input hit-tests a pointer against a rectangle without drawing one,
//! and the draw layer is only built when a renderer is. They live here so every
//! side names the same type instead of each keeping its own.

mod point;
mod rect;
mod transform;

pub use point::Pt;
pub use rect::Rect;
pub use transform::Transform;
