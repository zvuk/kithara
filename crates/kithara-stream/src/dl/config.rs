//! Downloader configuration.

use derive_setters::Setters;
use kithara_net::NetOptions;
use kithara_platform::{time::Duration, tokio::runtime::Handle};
use tokio_util::sync::CancellationToken;

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Setters)]
#[setters(prefix = "with_", strip_option)]
pub struct DownloaderConfig {
    /// Network options forwarded to the internal `HttpClient`.
    pub net: NetOptions,
    /// Cancellation token — when cancelled, the download loop exits.
    pub cancel: CancellationToken,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    #[setters(rename = "with_runtime")]
    pub runtime: Option<Handle>,
    /// Maximum number of concurrent in-flight fetch commands.
    pub max_concurrent: usize,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    pub demand_throttle: Duration,
    /// Soft timeout. When a fetch has not produced a response within
    /// this duration, the Downloader publishes
    /// [`DownloaderEvent::LoadSlow`](kithara_events::DownloaderEvent::LoadSlow)
    /// on the peer's bus (if any). The request itself is not aborted
    /// — it keeps running until hard timeout fires.
    pub soft_timeout: Duration,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        const MAX_CONCURRENT: usize = 5;
        const SOFT_TIMEOUT_SECS: u64 = 2;
        Self {
            net: NetOptions::default(),
            cancel: CancellationToken::new(),
            runtime: None,
            max_concurrent: MAX_CONCURRENT,
            demand_throttle: Duration::ZERO,
            soft_timeout: Duration::from_secs(SOFT_TIMEOUT_SECS),
        }
    }
}
