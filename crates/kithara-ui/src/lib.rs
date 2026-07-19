//! Serializable modular UI model for kithara.

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

pub use doc::{envelope, layout, module};

mod doc;
mod resolve;
mod validate;
