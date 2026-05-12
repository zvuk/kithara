use derive_setters::Setters;
use kithara_abr::AbrSettings;
use kithara_net::HttpClient;
use kithara_platform::{time::Duration, tokio::runtime::Handle};
use tokio_util::sync::CancellationToken;

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Setters)]
#[setters(prefix = "with_", strip_option)]
#[non_exhaustive]
pub struct DownloaderConfig {
    /// Settings for the shared ABR controller owned by the Downloader.
    pub abr_settings: AbrSettings,
    /// Cancellation token — when cancelled, the download loop exits.
    pub cancel: CancellationToken,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    pub demand_throttle: Duration,
    /// Soft timeout. When a fetch has not produced a response within
    /// this duration, the Downloader publishes
    /// [`DownloaderEvent::LoadSlow`](kithara_events::DownloaderEvent::LoadSlow)
    /// on the peer's bus (if any). The request itself is not aborted
    /// — it keeps running until hard timeout fires.
    pub soft_timeout: Duration,
    /// HTTP client used for all fetches. Cloned by the Downloader to
    /// share the underlying `reqwest::Client` (and its connection pool)
    /// with the caller. Pass a single shared `HttpClient` to multiple
    /// Downloaders to share keep-alive sockets across them; this is the
    /// production path. The default builds a fresh client from
    /// [`NetOptions::default()`](kithara_net::NetOptions) and is only
    /// appropriate for standalone tests.
    pub client: HttpClient,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    #[setters(rename = "with_runtime")]
    pub runtime: Option<Handle>,
    /// Maximum number of concurrent in-flight fetch commands.
    pub max_concurrent: usize,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        const MAX_CONCURRENT: usize = 5;
        const SOFT_TIMEOUT_SECS: u64 = 2;
        Self {
            client: HttpClient::default(),
            cancel: CancellationToken::new(), // kithara:cancel:owner
            runtime: None,
            max_concurrent: MAX_CONCURRENT,
            demand_throttle: Duration::ZERO,
            soft_timeout: Duration::from_secs(SOFT_TIMEOUT_SECS),
            abr_settings: AbrSettings::default(),
        }
    }
}
