//! Serializable modular UI model for kithara.

#[cfg(feature = "render")]
pub(crate) mod atoms;
pub mod builtin;
pub mod compile;
pub mod error;
pub mod expand;
pub mod ids;
pub mod registry;
#[cfg(feature = "render")]
pub mod render;
pub mod size;
pub mod source;
#[cfg(feature = "render")]
pub mod widgets;

pub use doc::{envelope, layout, module, skin};

mod doc;
mod resolve;
mod validate;
