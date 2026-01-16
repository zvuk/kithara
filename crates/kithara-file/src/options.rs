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
