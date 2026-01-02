use thiserror::Error;

#[derive(Debug, Error)]
pub enum OptionsError {
    #[error("Invalid seek position: {0}")]
    InvalidSeekPosition(u64),
}

#[derive(Clone, Debug, Default)]
pub struct FileSourceOptions {
    /// Maximum buffer size for streaming (bytes)
    pub max_buffer_size: Option<usize>,
    /// Whether to use range requests for seeking (when supported)
    pub enable_range_seek: bool,
    /// Timeout for network operations
    pub network_timeout: Option<std::time::Duration>,
    /// If true, only read from cache; fail if content not in cache
    pub offline_mode: bool,
}

impl FileSourceOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_buffer_size(mut self, size: usize) -> Self {
        self.max_buffer_size = Some(size);
        self
    }

    pub fn with_range_seek(mut self, enable: bool) -> Self {
        self.enable_range_seek = enable;
        self
    }

    pub fn with_network_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.network_timeout = Some(timeout);
        self
    }

    pub fn with_offline_mode(mut self, offline: bool) -> Self {
        self.offline_mode = offline;
        self
    }
}
