//! Photographing pages into files, and comparing two sets of photographs.
//!
//! A host that draws the same documents through two engines can only show that
//! they agree by photographing both and counting where the pixels disagree.
//! This crate owns everything about that which is not drawing: the walk over a
//! set of pages through a [`Stage`], the files a set is written as, cutting one
//! control out of a photograph, and the comparison of two sets. What
//! rasterises a page is the stage's business, so nothing here knows a toolkit.

pub mod diff;
mod film;
mod geometry;
mod part;
mod set;
mod stage;

pub use film::{Film, page_file};
pub use geometry::{Geometry, read_geometry, write_geometry, write_png};
pub use part::{Locate, Region, part_file, shoot_part};
pub use set::shoot_set;
pub use stage::Stage;
