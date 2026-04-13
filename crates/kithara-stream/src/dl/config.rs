//! Downloader configuration.

use kithara_net::NetOptions;
use kithara_platform::{time::Duration, tokio::runtime::Handle};
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
    /// Maximum number of concurrent in-flight fetch commands.
    pub max_concurrent: usize,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    pub demand_throttle: Duration,
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
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self {
            net: NetOptions::default(),
            cancel: CancellationToken::new(),
            runtime: None,
            max_concurrent: 5,
            demand_throttle: Duration::ZERO,
        }
    }
}
