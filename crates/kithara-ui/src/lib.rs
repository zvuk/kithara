//! Serializable modular UI model for kithara.

pub mod builtin;
pub mod compile;
pub mod error;
pub mod expand;
pub mod ids;
pub mod registry;
pub mod source;

pub use doc::{envelope, layout, module};

mod doc;
mod resolve;
mod ron_io;
mod validate;
