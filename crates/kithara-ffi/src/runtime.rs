//! Shared tokio runtime for FFI background tasks.

use std::sync::LazyLock;

use kithara_platform::tokio::runtime::{self, Builder as RuntimeBuilder};

/// Shared tokio runtime handle for FFI background tasks (event bridges, polling).
///
/// Runs a single-threaded tokio runtime on a dedicated OS thread.
/// Only requires the `rt` feature (no `rt-multi-thread`), compatible with iOS.
pub(crate) static FFI_RUNTIME: LazyLock<runtime::Handle> = LazyLock::new(|| {
    let rt = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("BUG: tokio current-thread runtime build cannot fail in normal startup");
    let handle = rt.handle().clone();
    kithara_platform::spawn(move || {
        rt.block_on(std::future::pending::<()>());
    });
    handle
});
