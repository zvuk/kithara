//! Worker item types with optional epoch tracking.

/// Fetch result from a source.
///
/// Returned by `fetch_next()` on worker sources.
#[derive(Debug, Clone)]
pub struct Fetch<C> {
    /// The data chunk.
    pub data: C,
    /// Whether this is the final chunk (EOF).
    pub is_eof: bool,
    /// Epoch for async sources (0 for sync sources).
    pub epoch: u64,
}

impl<C> Fetch<C> {
    /// Create a new fetch result.
    pub fn new(data: C, is_eof: bool, epoch: u64) -> Self {
        Self {
            data,
            is_eof,
            epoch,
        }
    }

    /// Check if this is an EOF marker.
    pub fn is_eof(&self) -> bool {
        self.is_eof
    }

    /// Get the epoch (0 for sync sources).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Consume and return the data.
    pub fn into_data(self) -> C {
        self.data
    }
}

/// Generic trait for worker items.
///
/// Allows different worker types to use different item metadata (e.g., with or without epochs).
pub trait WorkerItem: Send + 'static {
    /// The type of data contained in the item.
    type Data: Send + 'static;

    /// Get a reference to the data.
    fn data(&self) -> &Self::Data;

    /// Consume the item and return the data.
    fn into_data(self) -> Self::Data;

    /// Check if this is an EOF item.
    fn is_eof(&self) -> bool;
}

/// Item with epoch tracking for invalidation on seek.
///
/// Used by `AsyncWorker` for async sources that support seeking.
#[derive(Debug, Clone)]
pub struct EpochItem<C> {
    /// The data chunk.
    pub data: C,
    /// Epoch at which this item was fetched.
    pub epoch: u64,
    /// Whether this is the final item (EOF).
    pub is_eof: bool,
}

impl<C: Send + 'static> WorkerItem for EpochItem<C> {
    type Data = C;

    fn data(&self) -> &C {
        &self.data
    }

    fn into_data(self) -> C {
        self.data
    }

    fn is_eof(&self) -> bool {
        self.is_eof
    }
}

/// Simple item without epoch tracking.
///
/// Used by `SyncWorker` for synchronous sources that don't support seeking.
#[derive(Debug, Clone)]
pub struct SimpleItem<C> {
    /// The data chunk.
    pub data: C,
    /// Whether this is the final item (EOF).
    pub is_eof: bool,
}

impl<C: Send + 'static> WorkerItem for SimpleItem<C> {
    type Data = C;

    fn data(&self) -> &C {
        &self.data
    }

    fn into_data(self) -> C {
        self.data
    }

    fn is_eof(&self) -> bool {
        self.is_eof
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_item_trait() {
        let epoch_item = EpochItem {
            data: 42,
            epoch: 5,
            is_eof: false,
        };
        assert_eq!(*epoch_item.data(), 42);
        assert!(!epoch_item.is_eof());

        let simple_item = SimpleItem {
            data: 100,
            is_eof: true,
        };
        assert_eq!(*simple_item.data(), 100);
        assert!(simple_item.is_eof());

        let data = simple_item.into_data();
        assert_eq!(data, 100);
    }
}
