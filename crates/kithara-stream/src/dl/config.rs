use bon::Builder;
use kithara_abr::{AbrSettings, AbrSettingsPatch};
use kithara_macros::Patch;
use kithara_net::HttpClient;
use kithara_platform::{CancelToken, time::Duration, tokio::runtime::Handle};

/// Configuration for [`Downloader`](super::Downloader).
#[derive(Clone, Builder, Patch)]
#[builder(start_fn = for_client)]
#[non_exhaustive]
pub struct DownloaderConfig {
    /// HTTP client used for all fetches. Cloned by the Downloader to
    /// share the underlying `reqwest::Client` (and its connection pool)
    /// with the caller. Pass a single shared `HttpClient` to multiple
    /// Downloaders to share keep-alive sockets across them.
    #[builder(start_fn)]
    #[patch(skip)]
    pub(crate) client: HttpClient,
    /// Settings for the shared ABR controller owned by the Downloader.
    #[builder(default)]
    #[patch(nested)]
    pub(crate) abr_settings: AbrSettings,
    /// Throttle delay for demand (low-priority) processing.
    /// Gives urgent work a chance to preempt before demand batch runs.
    #[builder(default = Duration::ZERO)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) demand_throttle: Duration,
    /// Soft timeout. When a fetch has not produced a response within
    /// this duration, the Downloader publishes
    /// [`DownloaderEvent::LoadSlow`](kithara_events::DownloaderEvent::LoadSlow)
    /// on the peer's bus (if any). The request itself is not aborted
    /// — it keeps running until hard timeout fires.
    #[builder(default = Duration::from_secs(2))]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub(crate) soft_timeout: Duration,
    /// Optional parent cancel. `Some` → the download loop's scope is a child
    /// of it (composed); `None` → the Downloader owns a standalone scope. The
    /// `CancelScope` seam lives in [`Downloader::new`](super::Downloader::new).
    #[patch(skip)]
    pub(crate) cancel: Option<CancelToken>,
    /// Tokio runtime handle for the download loop.
    ///
    /// - `Some(handle)` — the loop runs as a task on this runtime.
    /// - `None` — spawns as a task on the current runtime via `task::spawn`.
    #[patch(skip)]
    pub(crate) runtime: Option<Handle>,
    /// Maximum number of concurrent in-flight fetch commands.
    #[builder(default = 5)]
    pub(crate) max_concurrent: usize,
    /// Capacity of the per-peer bounded command channel. A peer that fills
    /// it backpressures its own producer instead of the download loop, so
    /// this bounds the queue one peer may build ahead of the fetcher. The
    /// default is deep enough that a peer's planning burst does not block on
    /// the download loop, shallow enough that a stalled fetcher stops the
    /// producer rather than growing an unbounded backlog.
    #[builder(default = 32)]
    pub(crate) peer_cmd_channel_capacity: usize,
}

// Builds a real `HttpClient`, so Miri cannot reach it for the same reason it
// cannot reach `dl::tests`: the shared client initialises `aws-lc`, a C
// library Miri cannot enter.
#[cfg(all(test, not(miri)))]
mod tests {
    use kithara_abr::AbrSettings;
    use kithara_bufpool::testing::pools as test_pools;
    use kithara_net::{HttpClient, NetOptions};
    use kithara_platform::{CancelToken, time::Duration};
    use kithara_test_utils::kithara;

    use super::{DownloaderConfig, DownloaderConfigPatch};

    fn client() -> HttpClient {
        HttpClient::new(NetOptions::default(), test_pools(), CancelToken::never())
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_concurrency_it_names() {
        let patch: DownloaderConfigPatch =
            serde_yaml_ng::from_str("max_concurrent: 8\n").expect("the document types");
        // Seeded away from the built default of 2s, so the assertion below can
        // tell "left alone" from "reset to the default".
        let mut config = DownloaderConfig::for_client(client())
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

    #[kithara::test(native, flash(false))]
    fn a_nested_abr_patch_reaches_the_downloader() {
        let settings: DownloaderConfigPatch =
            serde_yaml_ng::from_str("abr_settings:\n  min_switch_interval: 45s\n")
                .expect("the document types");
        // The inner field is seeded away from its 0.8 default, so the second
        // assertion can tell "left alone" from "the nested struct was rebuilt
        // from `Default`" — the failure a nested apply is most likely to have.
        let mut config = DownloaderConfig::for_client(client())
            .abr_settings(AbrSettings::builder().down_hysteresis_ratio(0.55).build())
            .build();

        config.apply(settings);

        assert_eq!(
            config.abr_settings.min_switch_interval,
            Duration::from_secs(45)
        );
        assert!(
            (config.abr_settings.down_hysteresis_ratio - 0.55).abs() < f64::EPSILON,
            "a silent inner field must survive the nested apply"
        );
    }
}
