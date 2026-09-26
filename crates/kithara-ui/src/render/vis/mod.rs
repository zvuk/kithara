//! Visualiser reads, uniform packing, and toolkit-specific GPU adapters.

#[cfg(all(test, any(feature = "gpu", feature = "masonry")))]
mod fixture;
mod frame;
#[cfg(feature = "iced")]
mod iced;
#[cfg(feature = "masonry")]
mod masonry;
mod uniform;

pub(crate) use frame::VisFrame;
#[cfg(feature = "iced")]
pub(crate) use iced::view;
#[cfg(feature = "masonry")]
pub use masonry::{VisDeclaration, VisPass};
pub(crate) use uniform::{Uniforms, consts::SHADER};
