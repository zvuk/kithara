//! Serializable modular UI model for kithara.

pub mod app;
#[cfg(feature = "render")]
pub(crate) mod atoms;
#[cfg(any(feature = "render", feature = "vello"))]
pub mod backends;
pub mod builtin;
pub mod capture;
pub mod compile;
#[cfg(feature = "render")]
pub(crate) mod engine;
pub mod error;
pub mod expand;
pub mod ids;
pub(crate) mod mount;
pub mod registry;
#[cfg(feature = "render")]
pub mod render;
pub mod size;
#[cfg(feature = "render")]
pub(crate) mod solve;
pub mod source;
pub mod view;

pub use doc::{envelope, layout, module, package, param, skin, text};
#[cfg(feature = "render")]
pub use kithara_ui_draw as draw;
pub use kithara_ui_draw::geom;
#[cfg(feature = "render")]
pub use kithara_ui_input as interact;
#[cfg(feature = "render")]
pub use kithara_ui_lottie as lottie;
#[cfg(feature = "render")]
pub use kithara_ui_shaping as shaping;

mod doc;
mod require;
mod resolve;
mod room;
mod shader;
mod validate;
