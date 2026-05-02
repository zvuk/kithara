//! Shared tokio runtime for FFI background tasks.

use std::sync::LazyLock;

use kithara_platform::tokio::runtime::{self, Builder as RuntimeBuilder};

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
