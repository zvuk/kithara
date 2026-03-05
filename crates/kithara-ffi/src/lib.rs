//! Swift FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API using
//! cfg-switchable backends (`UniFFI` or `BoltFFI`).

use std::sync::LazyLock;

#[cfg(feature = "backend-uniffi")]
uniffi::setup_scaffolding!();

pub(crate) mod event_bridge;
pub mod item;
pub mod observer;
pub mod player;
pub mod types;

/// Shared tokio runtime handle for FFI background tasks (event bridge, time polling).
///
/// Runs a single-threaded tokio runtime on a dedicated OS thread.
/// Only requires the `rt` feature (no `rt-multi-thread`), compatible with iOS.
pub(crate) static FFI_RUNTIME: LazyLock<tokio::runtime::Handle> = LazyLock::new(|| {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("kithara-ffi".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create FFI tokio runtime");
            let _ = tx.send(rt.handle().clone());
            rt.block_on(std::future::pending::<()>());
        })
        .expect("failed to spawn FFI runtime thread");
    rx.recv().expect("failed to receive FFI runtime handle")
});
