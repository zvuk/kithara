#[cfg(not(target_arch = "wasm32"))]
#[path = "native.rs"]
mod platform;

#[cfg(target_arch = "wasm32")]
#[path = "wasm.rs"]
mod platform;

mod detector;

pub use detector::{HangDetector, HangDump, NoContext, default_timeout};
#[cfg(test)]
pub(crate) use detector::{fallback_timeout, sanitize_label};
#[cfg(test)]
pub(crate) use platform::{parse_timeout_secs, resolve_dump_dir, write_dump};

#[cfg(all(test, feature = "test-utils"))]
mod tests;
