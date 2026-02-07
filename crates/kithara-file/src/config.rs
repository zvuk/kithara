use std::path::PathBuf;

use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::FileEvent;

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
    /// File source (remote URL or local path).
    pub src: FileSrc,
    /// Optional name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (e.g. differ only in
    /// query parameters), setting a unique `name` ensures each gets its own
    /// cache directory.
    pub name: Option<String>,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Network configuration.
    pub net: NetOptions,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Events broadcast sender (optional - if not provided, events are not sent).
    pub events_tx: Option<broadcast::Sender<FileEvent>>,
    /// Events broadcast channel capacity (used when events_tx is not provided).
    pub events_channel_capacity: usize,
    /// Max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — pause when downloaded - read > n bytes (backpressure)
    /// - `None` — no backpressure, download as fast as possible
    pub look_ahead_bytes: Option<u64>,
    /// How often to yield to the async runtime during fast downloads.
    ///
    /// When `look_ahead_bytes` is `None`, the downloader yields after this many
    /// chunks to allow other tasks (like playback progress) to run.
    /// Default: 8 chunks.
    pub download_yield_interval: usize,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            src: FileSrc::Remote(
                Url::parse("http://localhost/audio.mp3").expect("valid default URL"),
            ),
            name: None,
            store: StoreOptions::default(),
            net: NetOptions::default(),
            cancel: None,
            events_tx: None,
            events_channel_capacity: 16,
            look_ahead_bytes: None,
            download_yield_interval: 8,
        }
    }
}

impl FileConfig {
    /// Create new file config with source.
    pub fn new(src: FileSrc) -> Self {
        Self {
            src,
            name: None,
            store: StoreOptions::default(),
            net: NetOptions::default(),
            cancel: None,
            events_tx: None,
            events_channel_capacity: 16,
            look_ahead_bytes: None,
            download_yield_interval: 8,
        }
    }

    /// Set name for cache disambiguation.
    ///
    /// When multiple URLs share the same canonical form (differ only in query
    /// parameters), a unique name ensures each gets its own cache directory.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set storage options.
    pub fn with_store(mut self, store: StoreOptions) -> Self {
        self.store = store;
        self
    }

    /// Set network options.
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set events sender for subscribing to file events.
    pub fn with_events(mut self, events_tx: broadcast::Sender<FileEvent>) -> Self {
        self.events_tx = Some(events_tx);
        self
    }

    /// Set events broadcast channel capacity.
    pub fn with_events_channel_capacity(mut self, capacity: usize) -> Self {
        self.events_channel_capacity = capacity;
        self
    }

    /// Set max bytes the downloader may be ahead of the reader before it pauses.
    ///
    /// - `Some(n)` — enable backpressure, pause when ahead by n bytes
    /// - `None` — disable backpressure, download as fast as possible
    pub fn with_look_ahead_bytes(mut self, bytes: Option<u64>) -> Self {
        self.look_ahead_bytes = bytes;
        self
    }

    /// Set how often to yield to the async runtime during fast downloads.
    ///
    /// When downloading without backpressure, the downloader yields after this
    /// many chunks to allow other tasks to run. Default: 8.
    pub fn with_download_yield_interval(mut self, interval: usize) -> Self {
        self.download_yield_interval = interval;
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
        assert!(config.events_tx.is_none());
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

        assert!(config.events_tx.is_none());
    }

    #[test]
    fn test_with_net() {
        let net = NetOptions::default();
        let config = FileConfig::new(test_src()).with_net(net);

        assert!(config.events_tx.is_none());
    }

    #[test]
    fn test_with_cancel() {
        let cancel = CancellationToken::new();
        let config = FileConfig::new(test_src()).with_cancel(cancel.clone());

        assert!(config.cancel.is_some());
    }

    #[test]
    fn test_with_events() {
        let (events_tx, _events_rx) = broadcast::channel(32);
        let config = FileConfig::new(test_src()).with_events(events_tx);

        assert!(config.events_tx.is_some());
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();
        let (events_tx, _) = broadcast::channel(32);

        let config = FileConfig::new(test_src())
            .with_store(store)
            .with_net(net)
            .with_cancel(cancel.clone())
            .with_events(events_tx);

        assert!(config.cancel.is_some());
        assert!(config.events_tx.is_some());
    }

    #[test]
    fn test_debug_impl() {
        let config = FileConfig::new(test_src());
        let debug_str = format!("{:?}", config);

        assert!(debug_str.contains("FileConfig"));
    }

    #[test]
    fn test_clone() {
        let (events_tx, _) = broadcast::channel(32);
        let config = FileConfig::new(test_src()).with_events(events_tx);

        let cloned = config.clone();

        assert!(cloned.events_tx.is_some());
    }
}
