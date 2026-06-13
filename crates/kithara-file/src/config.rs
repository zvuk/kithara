use std::{fmt, path::PathBuf, sync::Arc};

use bon::Builder;
use kithara_assets::{AssetStore, StoreOptions};
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_platform::CancelToken;
use kithara_stream::dl::Downloader;
use url::Url;

/// Source of a file stream: either a remote URL or a local path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileSrc {
    /// Remote file accessed via HTTP(S).
    Remote(Url),
    /// Local file accessed directly from disk.
    Local(PathBuf),
}

impl From<Url> for FileSrc {
    fn from(url: Url) -> Self {
        Self::Remote(url)
    }
}

impl From<PathBuf> for FileSrc {
    fn from(path: PathBuf) -> Self {
        Self::Local(path)
    }
}

/// Configuration for file streaming.
///
/// Used with `Stream::<File>::new(config)`.
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct FileConfig {
    /// File source (remote URL or local path).
    pub src: FileSrc,
    /// Event bus (optional - if not provided, one is created internally).
    #[builder(name = events)]
    pub bus: Option<EventBus>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancelToken>,
    /// Shared downloader (created lazily if not provided).
    pub downloader: Option<Downloader>,
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    pub name: Option<String>,
    /// Storage configuration.
    ///
    /// Honoured only when [`Self::asset_store`] is `None` (standalone
    /// mode). When the caller injects a shared `AssetStore` this field
    /// is ignored - the two are mutually exclusive modes, not a
    /// fallback chain.
    #[builder(default)]
    pub store: StoreOptions,
    /// Externally-owned shared `AssetStore<()>`. When `Some`, the file
    /// session reuses it and skips building a private store from
    /// [`Self::store`]. Lets several `Resource`s pointing at the same
    /// URL share one cached resource and availability surface.
    pub asset_store: Option<Arc<AssetStore<()>>>,
    /// Event bus channel capacity (used when `bus` is not provided).
    #[builder(default = kithara_events::DEFAULT_EVENT_BUS_CAPACITY)]
    pub event_channel_capacity: usize,
}

impl fmt::Debug for FileConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileConfig")
            .field("src", &self.src)
            .field("bus", &self.bus)
            .field("cancel", &self.cancel)
            .field("headers", &self.headers)
            .field("look_ahead_bytes", &self.look_ahead_bytes)
            .field("name", &self.name)
            .field("store", &self.store)
            .field("asset_store", &self.asset_store.is_some())
            .field("event_channel_capacity", &self.event_channel_capacity)
            .finish_non_exhaustive()
    }
}

impl Default for FileConfig {
    fn default() -> Self {
        let url = Url::parse("http://localhost/audio.mp3").expect("valid default URL");
        Self::for_src(FileSrc::Remote(url)).build()
    }
}

impl FileConfig {
    /// Create new file config with source.
    #[must_use]
    pub fn new(src: FileSrc) -> Self {
        Self::for_src(src).build()
    }

    /// Chainable counterpart to [`FileConfig::new`].
    pub fn for_src(src: FileSrc) -> FileConfigBuilder<file_config_builder::SetSrc> {
        Self::builder().src(src)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kithara_test_utils::kithara;

    use super::*;

    fn test_src() -> FileSrc {
        FileSrc::Remote(Url::parse("http://example.com/audio.mp3").unwrap())
    }

    #[kithara::test]
    #[case(test_src())]
    #[case(FileSrc::Local(PathBuf::from("/tmp/song.mp3")))]
    fn test_file_config_new_preserves_source(#[case] src: FileSrc) {
        let config = FileConfig::new(src.clone());

        assert_eq!(config.src, src);
        assert!(config.bus.is_none());
        assert!(config.cancel.is_none());
        if let FileSrc::Local(path) = &config.src {
            assert_eq!(path, Path::new("/tmp/song.mp3"));
        }
    }

    #[kithara::test]
    fn test_with_store() {
        let store = StoreOptions::default();
        let config = FileConfig::for_src(test_src()).store(store).build();

        assert!(config.bus.is_none());
    }

    fn apply_cancel(mut config: FileConfig) -> FileConfig {
        config.cancel = Some(CancelToken::never());
        config
    }

    fn apply_events(mut config: FileConfig) -> FileConfig {
        config.bus = Some(EventBus::new(32));
        config
    }

    fn apply_headers(mut config: FileConfig) -> FileConfig {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer token123");
        config.headers = Some(headers);
        config
    }

    fn has_cancel(config: &FileConfig) -> bool {
        config.cancel.is_some()
    }

    fn has_bus(config: &FileConfig) -> bool {
        config.bus.is_some()
    }

    fn has_auth_header(config: &FileConfig) -> bool {
        config.headers.as_ref().and_then(|h| h.get("Authorization")) == Some("Bearer token123")
    }

    #[kithara::test]
    #[case(apply_cancel, has_cancel)]
    #[case(apply_events, has_bus)]
    #[case(apply_headers, has_auth_header)]
    fn test_optional_setters_update_expected_field(
        #[case] apply: fn(FileConfig) -> FileConfig,
        #[case] check: fn(&FileConfig) -> bool,
    ) {
        let config = apply(FileConfig::new(test_src()));
        assert!(check(&config));
    }

    #[kithara::test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let cancel = CancelToken::never();
        let bus = EventBus::new(32);

        let config = FileConfig::for_src(test_src())
            .store(store)
            .cancel(cancel.clone())
            .events(bus)
            .build();

        assert!(config.cancel.is_some());
        assert!(config.bus.is_some());
    }

    #[kithara::test]
    #[case("stream-a")]
    #[case("stream-b")]
    fn test_with_name_sets_name(#[case] name: &str) {
        let config = FileConfig::for_src(test_src())
            .name(name.to_string())
            .build();
        assert_eq!(config.name.as_deref(), Some(name));
    }

    #[kithara::test]
    fn test_debug_impl() {
        let config = FileConfig::new(test_src());
        let debug_str = format!("{:?}", config);

        assert!(debug_str.contains("FileConfig"));
    }

    #[kithara::test]
    fn test_clone() {
        let bus = EventBus::new(32);
        let config = FileConfig::for_src(test_src()).events(bus).build();

        let cloned = config.clone();

        assert!(cloned.bus.is_some());
    }
}
