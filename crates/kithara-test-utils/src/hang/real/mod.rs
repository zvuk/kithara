#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod platform;

#[cfg(target_arch = "wasm32")]
#[path = "wasm.rs"]
mod platform;

mod shared;

#[cfg(not(target_arch = "wasm32"))]
#[path = "detector_native.rs"]
mod detector;

#[cfg(target_arch = "wasm32")]
#[path = "detector_wasm.rs"]
mod detector;

pub use detector::HangDetector;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use platform::{parse_timeout_secs, resolve_dump_dir, sanitize_label, write_dump};
pub use shared::{HangDump, NoContext, default_timeout};

#[cfg(test)]
mod tests;
