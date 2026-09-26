#[cfg(any(target_os = "macos", target_os = "ios"))]
mod accelerate;
mod portable;
mod traits;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use accelerate::Accelerate;
pub use portable::Portable;
pub use traits::{Backend, Platform};
