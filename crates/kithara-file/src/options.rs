use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::FileEvent;

/// Configuration for file streaming.
///
/// Used with `Stream::<File>::new(config)`.
#[derive(Clone, Debug)]
pub struct FileConfig {
    /// File URL.
    pub url: Url,
    /// Storage configuration.
    pub store: StoreOptions,
    /// Network configuration.
    pub net: NetOptions,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Events broadcast sender (optional - if not provided, events are not sent).
    pub events_tx: Option<broadcast::Sender<FileEvent>>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/audio.mp3").expect("valid default URL"),
            store: StoreOptions::default(),
            net: NetOptions::default(),
            cancel: None,
            events_tx: None,
        }
    }
}

impl FileConfig {
    /// Create new file config with URL.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            store: StoreOptions::default(),
            net: NetOptions::default(),
            cancel: None,
            events_tx: None,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_url() -> Url {
        Url::parse("http://example.com/audio.mp3").unwrap()
    }

    #[test]
    fn test_file_config_new() {
        let config = FileConfig::new(test_url());

        assert_eq!(config.url.as_str(), "http://example.com/audio.mp3");
        assert!(config.events_tx.is_none());
        assert!(config.cancel.is_none());
    }

    #[test]
    fn test_with_store() {
        let store = StoreOptions::default();
        let config = FileConfig::new(test_url()).with_store(store);

        assert!(config.events_tx.is_none());
    }

    #[test]
    fn test_with_net() {
        let net = NetOptions::default();
        let config = FileConfig::new(test_url()).with_net(net);

        assert!(config.events_tx.is_none());
    }

    #[test]
    fn test_with_cancel() {
        let cancel = CancellationToken::new();
        let config = FileConfig::new(test_url()).with_cancel(cancel.clone());

        assert!(config.cancel.is_some());
    }

    #[test]
    fn test_with_events() {
        let (events_tx, _events_rx) = broadcast::channel(32);
        let config = FileConfig::new(test_url()).with_events(events_tx);

        assert!(config.events_tx.is_some());
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();
        let (events_tx, _) = broadcast::channel(32);

        let config = FileConfig::new(test_url())
            .with_store(store)
            .with_net(net)
            .with_cancel(cancel.clone())
            .with_events(events_tx);

        assert!(config.cancel.is_some());
        assert!(config.events_tx.is_some());
    }

    #[test]
    fn test_debug_impl() {
        let config = FileConfig::new(test_url());
        let debug_str = format!("{:?}", config);

        assert!(debug_str.contains("FileConfig"));
    }

    #[test]
    fn test_clone() {
        let (events_tx, _) = broadcast::channel(32);
        let config = FileConfig::new(test_url()).with_events(events_tx);

        let cloned = config.clone();

        assert!(cloned.events_tx.is_some());
    }
}
