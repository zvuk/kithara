use std::path::PathBuf;

use bon::Builder;
use kithara_assets::AssetStore;
use kithara_bufpool::{HasPool, PoolRegion};
use kithara_derive::Patch;
use kithara_download::Downloader;
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_platform::{CancelToken, time::Duration};
use url::Url;

/// Source of a file stream: either a remote URL or a local path.
#[derive(Clone, Debug, derive_more::From, PartialEq, Eq)]
pub enum FileSrc {
    /// Remote file accessed via HTTP(S).
    Remote(Url),
    /// Local file accessed directly from disk.
    Local(PathBuf),
}

/// Configuration for file streaming.
///
/// Used with `Stream::<File<S>>::new(config)`.
#[kithara_config::config(construction, builder = false)]
#[derive(Builder, Patch)]
#[builder(on(String, into), start_fn = for_src)]
#[non_exhaustive]
#[derive_where::derive_where(Clone; S: HasPool<u8> + Send + Sync + 'static)]
#[derive(derive_more::Debug)]
pub struct FileConfig<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// File source (remote URL or local path).
    #[builder(start_fn)]
    #[patch(skip)]
    #[config(value)]
    pub src: FileSrc,
    /// Shared asset store used by local and remote sources.
    #[patch(skip)]
    #[config(skip = "injected asset store")]
    pub store: AssetStore<S>,
    /// Poll interval while a sibling `AssetStore` instance holds the
    /// atomic-chunked tmp for this file's canonical path. The default is short
    /// enough that the observed ~67 ms race window in
    /// `local_queue_playlist_behavior` resolves in a handful of ticks, long
    /// enough not to busy-spin a tokio worker.
    #[builder(default = Duration::from_millis(10))]
    #[patch(humantime)]
    #[config(value)]
    pub tmp_claim_poll_interval: Duration,
    /// Event bus (optional - if not provided, one is created internally).
    #[builder(name = events)]
    #[patch(skip)]
    #[config(skip = "injected event bus")]
    pub bus: Option<EventBus>,
    /// Cancellation token for graceful shutdown.
    #[patch(skip)]
    #[config(skip = "injected cancellation token")]
    pub cancel: Option<CancelToken>,
    /// Optional cache discriminator.
    #[patch(skip)]
    #[config(value)]
    pub discriminator: Option<String>,
    /// Shared downloader (created lazily if not provided).
    #[patch(skip)]
    #[debug(skip)]
    #[config(skip = "injected downloader")]
    pub downloader: Option<Downloader>,
    /// Explicit source-extension hint used before the URL-path extension.
    #[config(value)]
    pub extension: Option<String>,
    /// Additional HTTP headers to include in all requests.
    #[patch(skip)]
    #[config(value)]
    pub headers: Option<Headers>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    /// `None` permits fetching the whole file. `Some(0)` fetches only through
    /// the current read request.
    #[config(value, sdk(max = 8388608))]
    pub look_ahead_bytes: Option<u64>,
    /// Buffer-pool facade shared with storage and fallback transport.
    #[patch(skip)]
    #[config(skip = "injected buffer pools")]
    pub pools: PoolRegion<S>,
    /// Event bus channel capacity (used when `bus` is not provided).
    #[builder(default = kithara_events::DEFAULT_EVENT_BUS_CAPACITY)]
    #[config(value)]
    pub event_channel_capacity: usize,
    /// Ring depth for the decode-core to shell reader-event hand-off. A decode
    /// pass emits at most one progress event per decoded chunk, so the default
    /// bounds the worst-case post-seek skip burst without blocking the decode
    /// core.
    #[builder(default = 256)]
    #[config(value, sdk(max = 4096))]
    pub reader_event_capacity: usize,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    type TestConfig = FileConfig<TestPools>;
    type TestStore = AssetStore<TestPools>;

    fn test_src() -> FileSrc {
        FileSrc::Remote(Url::parse("http://example.com/audio.mp3").unwrap())
    }

    fn test_store() -> TestStore {
        AssetStore::builder(pools())
            .backend(StorageBackend::Memory)
            .build()
    }

    #[kithara::test]
    #[case(test_src())]
    #[case(FileSrc::Local(PathBuf::from("/tmp/song.mp3")))]
    fn test_file_config_for_src_preserves_source(#[case] src: FileSrc) {
        let config = FileConfig::for_src(src.clone())
            .store(test_store())
            .pools(pools())
            .build();

        assert_eq!(config.src, src);
        assert!(config.bus.is_none());
        assert!(config.cancel.is_none());
        if let FileSrc::Local(path) = &config.src {
            assert_eq!(path, Path::new("/tmp/song.mp3"));
        }
    }

    #[kithara::test]
    fn test_with_store() {
        let config = FileConfig::for_src(test_src())
            .store(test_store())
            .pools(pools())
            .build();

        assert!(config.bus.is_none());
    }

    fn apply_cancel(mut config: TestConfig) -> TestConfig {
        config.cancel = Some(CancelToken::never());
        config
    }

    fn apply_events(mut config: TestConfig) -> TestConfig {
        config.bus = Some(EventBus::new(32));
        config
    }

    fn apply_headers(mut config: TestConfig) -> TestConfig {
        let mut headers = Headers::default();
        headers.insert("Authorization", "Bearer token123");
        config.headers = Some(headers);
        config
    }

    fn has_cancel(config: &TestConfig) -> bool {
        config.cancel.is_some()
    }

    fn has_bus(config: &TestConfig) -> bool {
        config.bus.is_some()
    }

    fn has_auth_header(config: &TestConfig) -> bool {
        config.headers.as_ref().and_then(|h| h.get("Authorization")) == Some("Bearer token123")
    }

    #[kithara::test]
    #[case(apply_cancel, has_cancel)]
    #[case(apply_events, has_bus)]
    #[case(apply_headers, has_auth_header)]
    fn test_optional_setters_update_expected_field(
        #[case] apply: fn(TestConfig) -> TestConfig,
        #[case] check: fn(&TestConfig) -> bool,
    ) {
        let config = apply(
            FileConfig::for_src(test_src())
                .store(test_store())
                .pools(pools())
                .build(),
        );
        assert!(check(&config));
    }

    #[kithara::test]
    fn test_builder_chain() {
        let cancel = CancelToken::never();
        let bus = EventBus::new(32);

        let config = FileConfig::for_src(test_src())
            .store(test_store())
            .pools(pools())
            .cancel(cancel)
            .events(bus)
            .build();

        assert!(config.cancel.is_some());
        assert!(config.bus.is_some());
    }

    #[kithara::test]
    #[case("stream-a")]
    #[case("stream-b")]
    fn test_with_discriminator_sets_discriminator(#[case] name: &str) {
        let config = FileConfig::for_src(test_src())
            .store(test_store())
            .pools(pools())
            .discriminator(name)
            .build();
        assert_eq!(config.discriminator.as_deref(), Some(name));
    }

    #[kithara::test]
    fn test_debug_impl() {
        let config = FileConfig::for_src(test_src())
            .store(test_store())
            .pools(pools())
            .build();
        let debug_str = format!("{:?}", config);

        assert!(debug_str.contains("FileConfig"));
    }

    #[kithara::test]
    fn test_clone() {
        let bus = EventBus::new(32);
        let config = FileConfig::for_src(test_src())
            .store(test_store())
            .pools(pools())
            .events(bus)
            .build();

        let cloned = config.clone();

        assert!(config.bus.is_some());
        assert!(cloned.bus.is_some());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_test_utils::kithara;

    use super::{Duration, FileConfig, FileConfigPatch, FileSrc, Url};
    use crate::test_pools::{TestPools, pools};

    fn config() -> FileConfig<TestPools> {
        FileConfig::for_src(FileSrc::Remote(
            Url::parse("http://example.com/audio.mp3").expect("a literal URL parses"),
        ))
        .store(
            AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools())
        .build()
    }

    #[kithara::test(native, flash(false))]
    fn a_document_sets_the_reader_capacity_and_leaves_the_poll_interval() {
        let patch: FileConfigPatch =
            serde_yaml_ng::from_str("reader_event_capacity: 512\n").expect("the document types");
        // Seeded off the crate default so a merge that reset every unnamed
        // field could not pass this by coincidence.
        let mut config = config();
        config.tmp_claim_poll_interval = Duration::from_millis(77);

        config.apply(patch);

        assert_eq!(config.reader_event_capacity, 512);
        assert_eq!(
            config.tmp_claim_poll_interval,
            Duration::from_millis(77),
            "a key the document does not name must keep its seeded value"
        );
    }

    /// `reader_event_capacity_bytes` contains a real field name, which is
    /// what makes the assertion meaningful: serde's "expected one of" list
    /// names the real key, so a `contains` on the real name alone would pass
    /// vacuously.
    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error =
            serde_yaml_ng::from_str::<FileConfigPatch>("reader_event_capacity_bytes: 512\n")
                .expect_err("a typo must not be silently ignored");

        assert!(
            error.to_string().contains("reader_event_capacity_bytes"),
            "{error}"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_document_reads_the_poll_interval_from_humantime_text() {
        let patch: FileConfigPatch =
            serde_yaml_ng::from_str("tmp_claim_poll_interval: 25ms\n").expect("the document types");
        let mut config = config();

        config.apply(patch);

        assert_eq!(config.tmp_claim_poll_interval, Duration::from_millis(25));
    }

    /// The per-call wiring is not reachable from a document: naming it is
    /// refused rather than parsed and dropped.
    #[kithara::test(native, flash(false))]
    fn the_per_call_wiring_is_not_a_document_key() {
        let error = serde_yaml_ng::from_str::<FileConfigPatch>("discriminator: deck-0\n")
            .expect_err("per-call wiring is handed over in code, not named in a document");

        assert!(error.to_string().contains("discriminator"), "{error}");
    }
}
