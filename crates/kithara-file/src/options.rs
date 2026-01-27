use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::FileEvent;

/// Unified parameters for file streaming.
///
/// Used with `StreamSource::<File>::open(url, params)` or
/// `SyncReader::<StreamSource<File>>::open(url, params, reader_params)`.
#[derive(Clone, Debug)]
pub struct FileParams {
    /// Storage configuration (required).
    pub store: StoreOptions,
    /// Network configuration.
    pub net: NetOptions,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Events broadcast sender (optional - if not provided, events are not sent).
    pub events_tx: Option<broadcast::Sender<FileEvent>>,
}

impl Default for FileParams {
    fn default() -> Self {
        Self::new(StoreOptions::default())
    }
}

impl FileParams {
    /// Create new file params with the given store options.
    pub fn new(store: StoreOptions) -> Self {
        Self {
            store,
            net: NetOptions::default(),
            cancel: None,
            events_tx: None,
        }
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

    #[test]
    fn test_file_params_new() {
        let store = StoreOptions::default();
        let params = FileParams::new(store.clone());

        assert!(params.events_tx.is_none());
        assert!(params.cancel.is_none());
    }

    #[test]
    fn test_file_params_default() {
        let params = FileParams::default();

        assert!(params.events_tx.is_none());
        assert!(params.cancel.is_none());
    }

    #[test]
    fn test_with_net() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let params = FileParams::new(store).with_net(net);

        assert!(params.events_tx.is_none());
    }

    #[test]
    fn test_with_cancel() {
        let store = StoreOptions::default();
        let cancel = CancellationToken::new();
        let params = FileParams::new(store).with_cancel(cancel.clone());

        assert!(params.cancel.is_some());
    }

    #[test]
    fn test_with_events() {
        let store = StoreOptions::default();
        let (events_tx, _events_rx) = broadcast::channel(32);
        let params = FileParams::new(store).with_events(events_tx);

        assert!(params.events_tx.is_some());
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();
        let (events_tx, _) = broadcast::channel(32);

        let params = FileParams::new(store)
            .with_net(net)
            .with_cancel(cancel.clone())
            .with_events(events_tx);

        assert!(params.cancel.is_some());
        assert!(params.events_tx.is_some());
    }

    #[test]
    fn test_debug_impl() {
        let params = FileParams::default();
        let debug_str = format!("{:?}", params);

        assert!(debug_str.contains("FileParams"));
    }

    #[test]
    fn test_clone() {
        let (events_tx, _) = broadcast::channel(32);
        let params = FileParams::default().with_events(events_tx);

        let cloned = params.clone();

        assert!(cloned.events_tx.is_some());
    }
}
