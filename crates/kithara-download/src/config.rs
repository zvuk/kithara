use kithara_abr::{AbrSettings, AbrSettingsPatch};
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_net::HttpClient;
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::tokio::runtime::Handle;
use kithara_platform::{CancelToken, time::Duration};

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Config, Patch)]
#[config(builder(start_fn = for_client), fields(value))]
#[non_exhaustive]
pub struct DownloaderConfig {
    /// HTTP client used for all fetches. Cloned by the Downloader to
    /// share the underlying `reqwest::Client` (and its connection pool)
    /// with the caller. Pass a single shared `HttpClient` to multiple
    /// Downloaders to share keep-alive sockets across them.
    #[config(
        skip = "HTTP transport resource retained by the downloader",
        builder(start_fn),
        patch(skip)
    )]
    pub(crate) client: HttpClient,
    /// Settings for the shared ABR controller owned by the Downloader.
    #[config(
        skip = "transferred to the ABR controller",
        builder(default),
        patch(nested)
    )]
    pub(crate) abr_settings: AbrSettings,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    #[config(builder(default = Duration::ZERO), patch(humantime))]
    pub(crate) demand_throttle: Duration,
    /// Soft timeout. When a fetch has not produced a response within
    /// this duration, the Downloader publishes
    /// [`DownloaderEvent::LoadSlow`](crate::DownloaderEvent::LoadSlow)
    /// on the peer's bus (if any). The request itself is not aborted
    /// — it keeps running until hard timeout fires.
    #[config(builder(default = Duration::from_secs(2)), patch(humantime))]
    pub(crate) soft_timeout: Duration,
    /// Optional parent cancel. `Some` → the download loop's scope is a child
    /// of it (composed); `None` → the Downloader owns a standalone scope. The
    /// `CancelScope` seam lives in [`Downloader::new`](super::Downloader::new).
    #[config(skip = "composed into the downloader cancel scope", patch(skip))]
    pub(crate) cancel: Option<CancelToken>,
    /// Tokio runtime on which the download loop runs.
    #[cfg(not(target_arch = "wasm32"))]
    #[config(skip = "runtime resource retained by the downloader", patch(skip))]
    pub(crate) runtime: Handle,
    /// Maximum number of concurrent in-flight fetch commands.
    #[config(builder(default = 5))]
    pub(crate) max_concurrent: usize,
    /// Capacity of the per-peer bounded command channel. A peer that fills
    /// it backpressures its own producer instead of the download loop, so
    /// this bounds the queue one peer may build ahead of the fetcher. The
    /// default is deep enough that a peer's planning burst does not block on
    /// the download loop, shallow enough that a stalled fetcher stops the
    /// producer rather than growing an unbounded backlog.
    #[config(builder(default = 32))]
    pub(crate) peer_cmd_channel_capacity: usize,
}

/// Builds a real `HttpClient`, so Miri cannot reach it for the same reason it
/// cannot reach the download tests: the shared client initialises `aws-lc`, a C
/// library Miri cannot enter.
#[cfg(all(test, not(miri)))]
mod tests {
    use kithara_abr::AbrSettings;
    use kithara_config::Config;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::{CancelToken, time::Duration};
    use kithara_test_utils::{bufpool::pools as test_pools, kithara};

    use super::{DownloaderConfig, DownloaderConfigPatch};

    fn client() -> HttpClient {
        HttpClient::new(NetOptions::default(), test_pools(), CancelToken::never())
    }

    #[kithara::test(tokio, native, flash(false))]
    async fn a_patch_writes_only_the_concurrency_it_names() {
        let patch: DownloaderConfigPatch =
            serde_yaml_ng::from_str("max_concurrent: 8\n").expect("the document types");
        // Seeded away from the built default of 2s, so the assertion below can
        // tell "left alone" from "reset to the default".
        let mut config = DownloaderConfig::for_client(client())
            .runtime(kithara_platform::tokio::runtime::Handle::current())
            .soft_timeout(Duration::from_secs(9))
            .build();

        config.apply(patch);

        assert_eq!(config.max_concurrent, 8);
        assert_eq!(
            config.soft_timeout,
            Duration::from_secs(9),
            "a silent field must keep its value"
        );
    }

    #[kithara::test(tokio, native, flash(false))]
    async fn a_nested_abr_patch_reaches_the_downloader() {
        let settings: DownloaderConfigPatch =
            serde_yaml_ng::from_str("abr_settings:\n  min_switch_interval: 45s\n")
                .expect("the document types");
        // The inner field is seeded away from its 0.8 default, so the second
        // assertion can tell "left alone" from "the nested struct was rebuilt
        // from `Default`" — the failure a nested apply is most likely to have.
        let mut config = DownloaderConfig::for_client(client())
            .runtime(kithara_platform::tokio::runtime::Handle::current())
            .abr_settings(AbrSettings::builder().down_hysteresis_ratio(0.55).build())
            .build();

        config.apply(settings);

        assert_eq!(
            config.abr_settings.values().min_switch_interval,
            Duration::from_secs(45)
        );
        assert!(
            (config.abr_settings.values().down_hysteresis_ratio - 0.55).abs() < f64::EPSILON,
            "a silent inner field must survive the nested apply"
        );
    }
}
