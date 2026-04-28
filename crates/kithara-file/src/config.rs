use std::path::PathBuf;

use derivative::Derivative;
use derive_setters::Setters;
use kithara_assets::StoreOptions;
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_stream::dl::Downloader;
use tokio_util::sync::CancellationToken;
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
#[derive(Clone, Debug, Derivative, Setters)]
#[derivative(Default)]
#[setters(prefix = "with_", strip_option)]
pub struct FileConfig {
    /// File source (remote URL or local path).
    #[derivative(Default(
        value = "FileSrc::Remote(Url::parse(\"http://localhost/audio.mp3\").expect(\"valid default URL\"))"
    ))]
    pub src: FileSrc,
    /// Event bus (optional - if not provided, one is created internally).
    #[setters(rename = "with_events")]
    pub bus: Option<EventBus>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Shared downloader (created lazily if not provided).
    #[setters(skip)]
    #[derivative(Debug = "ignore")]
    pub downloader: Option<Downloader>,
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — pause when downloaded - read > n bytes (backpressure)
    /// - `None` — no backpressure, download as fast as possible
    pub look_ahead_bytes: Option<u64>,
    /// Optional name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (e.g. differ only in
    /// query parameters), setting a unique `name` ensures each gets its own
    /// cache directory.
    #[setters(skip)]
    pub name: Option<String>,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Event bus channel capacity (used when `bus` is not provided).
    #[derivative(Default(value = "kithara_events::DEFAULT_EVENT_BUS_CAPACITY"))]
    pub event_channel_capacity: usize,
}

impl FileConfig {
    /// Create new file config with source.
    #[must_use]
    pub fn new(src: FileSrc) -> Self {
        Self {
            src,
            ..Self::default()
        }
    }

    /// Set shared downloader.
    ///
    /// When not provided, a private downloader is created and spawned
    /// during [`StreamType::create`](kithara_stream::StreamType::create).
    #[must_use]
    pub fn with_downloader(mut self, dl: Downloader) -> Self {
        self.downloader = Some(dl);
        self
    }

    /// Set name for cache disambiguation.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
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
        let config = FileConfig::new(test_src()).with_store(store);

        assert!(config.bus.is_none());
    }

    fn apply_cancel(config: FileConfig) -> FileConfig {
        config.with_cancel(CancellationToken::new())
    }

    fn apply_events(config: FileConfig) -> FileConfig {
        config.with_events(EventBus::new(32))
    }

    fn apply_headers(config: FileConfig) -> FileConfig {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer token123");
        config.with_headers(headers)
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
        let cancel = CancellationToken::new();
        let bus = EventBus::new(32);

        let config = FileConfig::new(test_src())
            .with_store(store)
            .with_cancel(cancel.clone())
            .with_events(bus);

        assert!(config.cancel.is_some());
        assert!(config.bus.is_some());
    }

    #[kithara::test]
    #[case("stream-a")]
    #[case("stream-b")]
    fn test_with_name_sets_name(#[case] name: &str) {
        let config = FileConfig::new(test_src()).with_name(name);
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
        let config = FileConfig::new(test_src()).with_events(bus);

        let cloned = config.clone();

        assert!(cloned.bus.is_some());
    }
}
