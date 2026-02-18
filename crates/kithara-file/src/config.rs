use std::path::PathBuf;

use kithara_assets::StoreOptions;
use kithara_events::EventBus;
use kithara_net::{Headers, NetOptions};
use kithara_platform::ThreadPool;
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
#[derive(Clone, Debug)]
pub struct FileConfig {
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Event bus channel capacity (used when `bus` is not provided).
    pub event_channel_capacity: usize,
    /// Event bus (optional - if not provided, one is created internally).
    pub bus: Option<EventBus>,
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
    pub name: Option<String>,
    /// Additional HTTP headers to include in all requests.
    pub headers: Option<Headers>,
    /// Network configuration.
    pub net: NetOptions,
    /// File source (remote URL or local path).
    pub src: FileSrc,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Thread pool for background work.
    ///
    /// Shared across all components. When `None`, defaults to the global rayon pool.
    pub thread_pool: Option<ThreadPool>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            cancel: None,

            event_channel_capacity: 16,
            bus: None,
            headers: None,
            look_ahead_bytes: None,
            name: None,
            net: NetOptions::default(),
            src: FileSrc::Remote(
                Url::parse("http://localhost/audio.mp3").expect("valid default URL"),
            ),
            store: StoreOptions::default(),
            thread_pool: None,
        }
    }
}

impl FileConfig {
    /// Create new file config with source.
    #[must_use]
    pub fn new(src: FileSrc) -> Self {
        Self {
            cancel: None,

            event_channel_capacity: 16,
            bus: None,
            headers: None,
            look_ahead_bytes: None,
            name: None,
            net: NetOptions::default(),
            src,
            store: StoreOptions::default(),
            thread_pool: None,
        }
    }

    /// Set name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (differ only in query
    /// parameters), a unique name ensures each gets its own cache directory.
    pub fn with_name<S: Into<String>>(mut self, name: S) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set storage options.
    #[must_use]
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }

    /// Set network options.
    #[must_use]
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set additional HTTP headers for all requests.
    #[must_use]
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = Some(headers);
        self
    }

    /// Set cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set event bus for subscribing to file events.
    #[must_use]
    pub fn with_events(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Set event bus channel capacity.
    #[must_use]
    pub fn with_event_channel_capacity(mut self, capacity: usize) -> Self {
        self.event_channel_capacity = capacity;
        self
    }

    /// Set max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — enable backpressure, pause when ahead by n bytes
    /// - `None` — disable backpressure, download as fast as possible
    #[must_use]
    pub fn with_look_ahead_bytes(mut self, bytes: Option<u64>) -> Self {
        self.look_ahead_bytes = bytes;
        self
    }

    /// Set thread pool for background work.
    ///
    /// The pool is shared across all components. When not set, defaults to the
    /// global rayon pool.
    #[must_use]
    pub fn with_thread_pool(mut self, pool: ThreadPool) -> Self {
        self.thread_pool = Some(pool);
        self
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn test_src() -> FileSrc {
        FileSrc::Remote(Url::parse("http://example.com/audio.mp3").unwrap())
    }

    #[test]
    fn test_file_config_new() {
        let config = FileConfig::new(test_src());

        assert!(
            matches!(&config.src, FileSrc::Remote(url) if url.as_str() == "http://example.com/audio.mp3")
        );
        assert!(config.bus.is_none());
        assert!(config.cancel.is_none());
    }

    #[test]
    fn test_local_src() {
        let config = FileConfig::new(FileSrc::Local(PathBuf::from("/tmp/song.mp3")));

        assert!(matches!(&config.src, FileSrc::Local(p) if p == Path::new("/tmp/song.mp3")));
    }

    #[test]
    fn test_with_store() {
        let store = StoreOptions::default();
        let config = FileConfig::new(test_src()).with_store(store);

        assert!(config.bus.is_none());
    }

    #[test]
    fn test_with_net() {
        let net = NetOptions::default();
        let config = FileConfig::new(test_src()).with_net(net);

        assert!(config.bus.is_none());
    }

    #[test]
    fn test_with_cancel() {
        let cancel = CancellationToken::new();
        let config = FileConfig::new(test_src()).with_cancel(cancel.clone());

        assert!(config.cancel.is_some());
    }

    #[test]
    fn test_with_events() {
        let bus = EventBus::new(32);
        let config = FileConfig::new(test_src()).with_events(bus);

        assert!(config.bus.is_some());
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();
        let bus = EventBus::new(32);

        let config = FileConfig::new(test_src())
            .with_store(store)
            .with_net(net)
            .with_cancel(cancel.clone())
            .with_events(bus);

        assert!(config.cancel.is_some());
        assert!(config.bus.is_some());
    }

    #[test]
    fn test_with_headers() {
        let mut headers = Headers::new();
        headers.insert("Authorization", "Bearer token123");
        let config = FileConfig::new(test_src()).with_headers(headers);

        assert!(config.headers.is_some());
        assert_eq!(
            config.headers.as_ref().and_then(|h| h.get("Authorization")),
            Some("Bearer token123")
        );
    }

    #[test]
    fn test_debug_impl() {
        let config = FileConfig::new(test_src());
        let debug_str = format!("{:?}", config);

        assert!(debug_str.contains("FileConfig"));
    }

    #[test]
    fn test_clone() {
        let bus = EventBus::new(32);
        let config = FileConfig::new(test_src()).with_events(bus);

        let cloned = config.clone();

        assert!(cloned.bus.is_some());
    }
}
