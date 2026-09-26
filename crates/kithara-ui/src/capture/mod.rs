//! Photographing a page of this toolkit with no window.
//!
//! The walk over a set of pages, the files it writes and the comparison of two
//! sets are `kithara-ui-capture`'s; what lives here is what rasterises a page
//! on each host, which only this crate can do. It is behind the `capture`
//! feature so a shipped build carries none of it.
//!
//! The feature gate stands here rather than on the declaration in `lib.rs`,
//! the way `app` carries its own: one gate on the module is one gate, and the
//! crate root already carries as many as it can hold.
#![cfg(feature = "capture")]

mod photo;
#[cfg(feature = "masonry")]
mod scene;

pub use kithara_ui_capture::*;
pub use photo::Photographer;
#[cfg(feature = "masonry")]
pub use scene::Offscreen;
