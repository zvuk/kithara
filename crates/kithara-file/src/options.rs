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
    /// Maximum buffer size for streaming (bytes).
    pub max_buffer_size: Option<usize>,
    /// Cancellation token for graceful shutdown.
    pub cancel: Option<CancellationToken>,
    /// Capacity of the events broadcast channel.
    pub event_capacity: usize,
    /// Capacity of the command mpsc channel.
    pub command_capacity: usize,
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
            max_buffer_size: None,
            cancel: None,
            event_capacity: 32,
            command_capacity: 8,
        }
    }

    /// Set network options.
    pub fn with_net(mut self, net: NetOptions) -> Self {
        self.net = net;
        self
    }

    /// Set maximum buffer size.
    pub fn with_max_buffer_size(mut self, size: usize) -> Self {
        self.max_buffer_size = Some(size);
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

    /// Set command channel capacity.
    pub fn with_command_capacity(mut self, capacity: usize) -> Self {
        self.command_capacity = capacity;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;

    #[test]
    fn test_file_params_new() {
        let store = StoreOptions::default();
        let params = FileParams::new(store.clone());

        assert_eq!(params.event_capacity, 32);
        assert_eq!(params.command_capacity, 8);
        assert_eq!(params.max_buffer_size, None);
        assert!(params.cancel.is_none());
    }

    #[test]
    fn test_file_params_default() {
        let params = FileParams::default();

        assert_eq!(params.event_capacity, 32);
        assert_eq!(params.command_capacity, 8);
        assert_eq!(params.max_buffer_size, None);
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

    #[rstest]
    #[case(1024)]
    #[case(4096)]
    #[case(8192)]
    #[case(1024 * 1024)]
    fn test_with_max_buffer_size(#[case] size: usize) {
        let store = StoreOptions::default();
        let params = FileParams::new(store).with_max_buffer_size(size);

        assert_eq!(params.max_buffer_size, Some(size));
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

    #[rstest]
    #[case(1)]
    #[case(4)]
    #[case(16)]
    #[case(32)]
    fn test_with_command_capacity(#[case] capacity: usize) {
        let store = StoreOptions::default();
        let params = FileParams::new(store).with_command_capacity(capacity);

        assert_eq!(params.command_capacity, capacity);
    }

    #[test]
    fn test_builder_chain() {
        let store = StoreOptions::default();
        let net = NetOptions::default();
        let cancel = CancellationToken::new();

        let params = FileParams::new(store)
            .with_net(net)
            .with_max_buffer_size(8192)
            .with_cancel(cancel.clone())
            .with_event_capacity(64)
            .with_command_capacity(16);

        assert_eq!(params.max_buffer_size, Some(8192));
        assert!(params.cancel.is_some());
        assert_eq!(params.event_capacity, 64);
        assert_eq!(params.command_capacity, 16);
    }

    #[test]
    fn test_debug_impl() {
        let params = FileParams::default();
        let debug_str = format!("{:?}", params);

        assert!(debug_str.contains("FileParams"));
    }

    #[test]
    fn test_clone() {
        let params = FileParams::default()
            .with_max_buffer_size(4096)
            .with_event_capacity(64);

        let cloned = params.clone();

        assert_eq!(cloned.max_buffer_size, params.max_buffer_size);
        assert_eq!(cloned.event_capacity, params.event_capacity);
        assert_eq!(cloned.command_capacity, params.command_capacity);
    }
}
