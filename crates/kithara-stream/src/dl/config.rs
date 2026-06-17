use bon::Builder;
use kithara_abr::AbrSettings;
use kithara_net::HttpClient;
use kithara_platform::{CancelToken, time::Duration, tokio::runtime::Handle};

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Builder)]
#[non_exhaustive]
pub struct DownloaderConfig {
    /// Settings for the shared ABR controller owned by the Downloader.
    #[builder(default)]
    pub abr_settings: AbrSettings,
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
    /// Downloaders to share keep-alive sockets across them.
    pub client: HttpClient,
    /// Optional parent cancel. `Some` → the download loop's scope is a child
    /// of it (composed); `None` → the Downloader owns a standalone scope. The
    /// `CancelScope` seam lives in [`Downloader::new`](super::Downloader::new).
    pub cancel: Option<CancelToken>,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    pub runtime: Option<Handle>,
    /// Maximum number of concurrent in-flight fetch commands.
    #[builder(default = 5)]
    pub max_concurrent: usize,
}

impl DownloaderConfig {
    /// Start a builder with `client` already set. Mirrors the
    /// `ResourceConfig::for_src` / `HlsConfig::for_url` pattern — the
    /// one required field is pre-supplied so callers can chain only
    /// the optional knobs they care about.
    pub fn for_client(
        client: HttpClient,
    ) -> DownloaderConfigBuilder<downloader_config_builder::SetClient> {
        Self::builder().client(client)
    }
}
