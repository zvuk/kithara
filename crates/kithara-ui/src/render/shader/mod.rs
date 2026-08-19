//! Document shader snapshots and toolkit-specific GPU adapters.

mod frame;
#[cfg(feature = "iced")]
mod iced;
#[cfg(feature = "masonry")]
mod masonry;
#[cfg(test)]
mod tests;

#[cfg(feature = "masonry")]
pub(crate) use frame::ShaderFrameError;
pub(crate) use frame::{ShaderFrame, logical_extent};
#[cfg(feature = "iced")]
pub(crate) use iced::view;
#[cfg(feature = "masonry")]
pub use masonry::{ShaderDeclaration, ShaderPass};
