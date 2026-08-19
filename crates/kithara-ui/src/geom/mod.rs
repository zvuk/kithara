//! Toolkit-neutral geometry.
//!
//! A point and an affine transform are not drawing commands: expansion folds a
//! document's poses into a transform long before anything is painted, and the
//! draw layer is only built when a renderer is. They live here so both sides
//! name the same type instead of each keeping its own.

mod point;
mod transform;

pub use point::Pt;
pub use transform::Transform;
