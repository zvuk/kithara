//! Android platform ABI and safe wrappers shared by Kithara crates.
//!
//! The crate owns the raw NDK media bindings, the wrappers over them, and
//! access to the host runtime handle: it publishes the `JavaVM` and the
//! application context, and attaches calling threads to them. It also installs
//! the host application's HTTP transport into `kithara-net` and carries the
//! JNI glue that runs Kithara's requests through it. Decode and format policy
//! live in `kithara-decode`, the player's `Java_*` entry points in
//! `kithara-ffi`.

#[cfg(target_os = "android")]
mod buffer;
mod error;
#[cfg(target_os = "android")]
mod http;
// The media bindings carry `#[link(name = "mediandk")]`, which reaches the
// link of every artifact that compiles them.
#[cfg(target_os = "android")]
pub mod media;
#[cfg(target_os = "android")]
mod method;
mod runtime;

pub use error::AndroidBackendError;
pub use runtime::{attach_current_thread, initialize};
