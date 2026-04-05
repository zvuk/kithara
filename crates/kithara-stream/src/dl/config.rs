//! Downloader configuration.

use kithara_bufpool::BytePool;
use kithara_net::NetOptions;
use kithara_platform::tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone)]
pub struct DownloaderConfig {
    /// Network options forwarded to the internal `HttpClient`.
    pub net: NetOptions,
    /// Cancellation token — when cancelled, the download loop exits.
    pub cancel: CancellationToken,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    pub runtime: Option<Handle>,
    /// Byte buffer pool for `FetchMethod::Get` body accumulation.
    ///
    /// - `Some(pool)` — use this pool (tests can inject an isolated pool).
    /// - `None` — falls back to the global `byte_pool()`.
    pub pool: Option<BytePool>,
}

impl DownloaderConfig {
    /// Set network options.
    #[must_use]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Set an explicit runtime handle.
    #[must_use]
    pub fn with_runtime(mut self, handle: Handle) -> Self {
        self.runtime = Some(handle);
        self
    }

    /// Set a custom byte pool (for testing isolation).
    #[must_use]
    pub fn with_pool(mut self, pool: BytePool) -> Self {
        self.pool = Some(pool);
        self
    }
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self {
            net: NetOptions::default(),
            cancel: CancellationToken::new(),
            runtime: None,
            pool: None,
        }
    }
}
