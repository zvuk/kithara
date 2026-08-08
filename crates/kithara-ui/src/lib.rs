//! Serializable modular UI model for kithara.

pub mod app;
#[cfg(feature = "render")]
pub(crate) mod atoms;
#[cfg(any(feature = "render", feature = "vello-backend"))]
pub mod backends;
pub mod builtin;
pub mod compile;
#[cfg(any(feature = "render", feature = "vello-backend"))]
pub mod draw;
#[cfg(feature = "render")]
pub(crate) mod engine;
pub mod error;
pub mod expand;
pub mod ids;
#[cfg(feature = "render")]
pub mod interact;
pub(crate) mod mount;
pub mod registry;
#[cfg(feature = "render")]
pub mod render;
pub mod size;
#[cfg(feature = "render")]
pub(crate) mod solve;
pub mod source;
#[cfg(any(feature = "render", feature = "vello-backend"))]
pub mod text;
#[cfg(feature = "render")]
pub mod widgets;

pub use doc::{envelope, layout, module, param, skin};

mod doc;
mod resolve;
mod validate;
