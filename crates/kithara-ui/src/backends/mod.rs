#[cfg(all(test, feature = "masonry"))]
pub(crate) mod conformance;
#[cfg(feature = "iced")]
mod iced_canvas;
#[cfg(feature = "vello")]
mod image;
#[cfg(all(test, feature = "masonry"))]
mod lottie;
#[cfg(all(feature = "masonry", any(test, feature = "capture")))]
mod readback;
#[cfg(feature = "vello")]
mod vello;

#[cfg(feature = "iced")]
pub(crate) use iced_canvas::{font, path, replay_ordered, replay_ordered_in};
#[cfg(feature = "vello")]
pub(crate) use image::VelloImageBackend;
#[cfg(all(feature = "masonry", any(test, feature = "capture")))]
pub(crate) use readback::read_back;
#[cfg(feature = "vello")]
pub use vello::{VelloBackend, paint_color};
