use std::time::Duration;

use thiserror::Error;

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
