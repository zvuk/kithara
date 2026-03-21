//! Cross-platform FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API and lets `UniFFI`
//! generate platform bindings such as Swift and Kotlin.

use std::sync::LazyLock;

use kithara_platform::{
    sync::mpsc,
    tokio::runtime::{self, Builder as RuntimeBuilder},
};

#[cfg(feature = "backend-uniffi")]
uniffi::setup_scaffolding!();

#[cfg(target_os = "android")]
pub(crate) mod android;
pub mod config;
pub(crate) mod event_bridge;
pub mod item;
pub(crate) mod item_bridge;
pub mod observer;
pub mod player;
pub mod types;

/// Shared tokio runtime handle for FFI background tasks (event bridges, polling).
///
/// Runs a single-threaded tokio runtime on a dedicated OS thread.
/// Only requires the `rt` feature (no `rt-multi-thread`), compatible with iOS.
pub(crate) static FFI_RUNTIME: LazyLock<runtime::Handle> = LazyLock::new(|| {
    let (tx, rx) = mpsc::channel();
    kithara_platform::spawn(move || {
        let rt = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create FFI tokio runtime");
        let _ = tx.send_sync(rt.handle().clone());
        rt.block_on(std::future::pending::<()>());
    });
    rx.recv_sync()
        .expect("failed to receive FFI runtime handle")
});
