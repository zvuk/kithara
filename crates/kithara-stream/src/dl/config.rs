use bon::Builder;
use kithara_abr::AbrSettings;
use kithara_net::HttpClient;
use kithara_platform::{time::Duration, tokio::runtime::Handle};
use tokio_util::sync::CancellationToken;

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Builder)]
#[non_exhaustive]
pub struct DownloaderConfig {
    /// Settings for the shared ABR controller owned by the Downloader.
    #[builder(default)]
    pub abr_settings: AbrSettings,
    /// Cancellation token — when cancelled, the download loop exits.
    #[builder(default = CancellationToken::new())] // kithara:cancel:owner
    pub cancel: CancellationToken,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    #[builder(default = Duration::ZERO)]
    pub demand_throttle: Duration,
    /// Soft timeout. When a fetch has not produced a response within
    /// this duration, the Downloader publishes
    /// [`DownloaderEvent::LoadSlow`](kithara_events::DownloaderEvent::LoadSlow)
    /// on the peer's bus (if any). The request itself is not aborted
    /// — it keeps running until hard timeout fires.
    #[builder(default = Duration::from_secs(2))]
    pub soft_timeout: Duration,
    /// HTTP client used for all fetches. Cloned by the Downloader to
    /// share the underlying `reqwest::Client` (and its connection pool)
    /// with the caller. Pass a single shared `HttpClient` to multiple
    /// Downloaders to share keep-alive sockets across them; this is the
    /// production path. The default builds a fresh client from
    /// [`NetOptions::default()`](kithara_net::NetOptions) and is only
    /// appropriate for standalone tests.
    #[builder(default)]
    pub client: HttpClient,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    pub runtime: Option<Handle>,
    /// Maximum number of concurrent in-flight fetch commands.
    #[builder(default = 5)]
    pub max_concurrent: usize,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self::builder().build()
    }
}
