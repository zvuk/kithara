//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API and lets `UniFFI`
//! generate platform bindings such as Swift and Kotlin.

use std::sync::LazyLock;

use kithara_platform::tokio::runtime::{self, Builder as RuntimeBuilder};

#[cfg(feature = "backend-uniffi")]
uniffi::setup_scaffolding!();

#[cfg(target_os = "android")]
pub(crate) mod android;
#[cfg(all(target_os = "android", feature = "test"))]
pub(crate) mod android_test;
pub mod cipher;
pub mod config;
pub(crate) mod event_bridge;
pub mod item;
pub(crate) mod item_bridge;
pub mod logging;
pub mod observer;
pub mod player;
pub mod types;

/// Shared tokio runtime handle for FFI background tasks (event bridges, polling).
///
/// Runs a single-threaded tokio runtime on a dedicated OS thread.
/// Only requires the `rt` feature (no `rt-multi-thread`), compatible with iOS.
pub(crate) static FFI_RUNTIME: LazyLock<runtime::Handle> = LazyLock::new(|| {
    // Build the runtime on the caller's thread to obtain its handle without
    // a cross-thread handshake. Blocking on an mpsc receiver here would
    // cause a priority inversion when the first caller is a UI thread
    // (User-interactive QoS) waiting on a default-QoS worker.
    let rt = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to create FFI tokio runtime");
    let handle = rt.handle().clone();
    kithara_platform::spawn(move || {
        // Keep the runtime alive indefinitely so its `Handle` remains valid.
        rt.block_on(std::future::pending::<()>());
    });
    handle
});
