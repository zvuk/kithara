use std::time::Duration;

use kithara_assets::StoreOptions;
use kithara_net::NetOptions;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum OptionsError {
    #[error("Invalid seek position: {0}")]
    InvalidSeekPosition(u64),
}

/// Options for file source streaming.
/// Range seek is always enabled; only buffer and timeout are configurable here.
#[derive(Clone, Debug, Default)]
pub struct FileSourceOptions {
    /// Maximum buffer size for streaming (bytes)
    pub max_buffer_size: Option<usize>,
    /// Timeout for network operations
    pub network_timeout: Option<Duration>,
}

impl FileSourceOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_buffer_size(mut self, size: usize) -> Self {
        self.max_buffer_size = Some(size);
        self
    }

    pub fn with_network_timeout(mut self, timeout: Duration) -> Self {
        self.network_timeout = Some(timeout);
        self
    }
}

/// Unified parameters for file streaming.
///
/// Used with `Stream::<File>::open(url, params)` for the unified API.
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
}
