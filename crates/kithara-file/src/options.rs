use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use tokio_util::sync::CancellationToken;

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
    /// Capacity of the events broadcast channel.
    pub event_capacity: usize,
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
            event_capacity: 32,
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

    /// Set event channel capacity.
    pub fn with_event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    #[test]
    fn test_file_params_new() {
        let store = StoreOptions::default();
        let params = FileParams::new(store.clone());

        assert_eq!(params.event_capacity, 32);
        assert!(params.cancel.is_none());
    }

    #[test]
    fn test_file_params_default() {
        let params = FileParams::default();

        assert_eq!(params.event_capacity, 32);
        assert!(params.cancel.is_none());
    }

    #[test]
    fn test_with_net() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let params = FileParams::new(store).with_net(net);

        // NetOptions doesn't impl PartialEq, just verify it compiles
        assert_eq!(params.event_capacity, 32);
    }

    #[test]
    fn test_with_cancel() {
        let store = StoreOptions::default();
        let cancel = CancellationToken::new();
        let params = FileParams::new(store).with_cancel(cancel.clone());

        assert!(params.cancel.is_some());
    }

    #[rstest]
    #[case(1)]
    #[case(16)]
    #[case(64)]
    #[case(128)]
    fn test_with_event_capacity(#[case] capacity: usize) {
        let store = StoreOptions::default();
        let params = FileParams::new(store).with_event_capacity(capacity);

        assert_eq!(params.event_capacity, capacity);
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();

        let params = FileParams::new(store)
            .with_net(net)
            .with_cancel(cancel.clone())
            .with_event_capacity(64);

        assert!(params.cancel.is_some());
        assert_eq!(params.event_capacity, 64);
    }

    #[test]
    fn test_debug_impl() {
        let params = FileParams::default();
        let debug_str = format!("{:?}", params);

        assert!(debug_str.contains("FileParams"));
    }

    #[test]
    fn test_clone() {
        let params = FileParams::default().with_event_capacity(64);

        let cloned = params.clone();

        assert_eq!(cloned.event_capacity, params.event_capacity);
    }
}
