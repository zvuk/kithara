//! Serializable modular UI model for kithara.

pub mod app;
#[cfg(feature = "render")]
pub(crate) mod atoms;
#[cfg(any(feature = "render", feature = "vello"))]
pub mod backends;
pub mod builtin;
pub mod compile;
#[cfg(any(feature = "render", feature = "vello"))]
pub mod draw;
#[cfg(feature = "render")]
pub(crate) mod engine;
pub mod error;
pub mod expand;
pub mod geom;
pub mod ids;
#[cfg(feature = "render")]
pub mod interact;
#[cfg(feature = "render")]
pub mod lottie;
pub(crate) mod mount;
pub mod registry;
#[cfg(feature = "render")]
pub mod render;
#[cfg(any(feature = "render", feature = "vello"))]
pub mod shaping;
pub mod size;
#[cfg(feature = "render")]
pub(crate) mod solve;
pub mod source;

pub use doc::{envelope, layout, module, param, skin, text};

mod doc;
mod resolve;
mod shader;
mod validate;
