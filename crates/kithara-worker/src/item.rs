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

    /// Consume the fetch and return the inner data.
    ///
    /// This is the idiomatic way to extract owned data from the wrapper,
    /// following the Rust stdlib pattern (e.g., `Arc::into_inner()`, `Box::into_inner()`).
    pub fn into_inner(self) -> C {
        self.data
    }

    /// Check if this is an EOF marker.
    pub fn is_eof(&self) -> bool {
        self.is_eof
    }

    /// Get the epoch (0 for sync sources).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl<C> AsRef<C> for Fetch<C> {
    fn as_ref(&self) -> &C {
        &self.data
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

/// Implement WorkerItem for Fetch.
impl<C: Send + 'static> WorkerItem for Fetch<C> {
    type Data = C;

    fn data(&self) -> &C {
        &self.data
    }

    fn into_data(self) -> C {
        self.into_inner()
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
        let epoch_item = Fetch::new(42, false, 5);
        assert_eq!(*epoch_item.data(), 42);
        assert!(!epoch_item.is_eof());
        assert_eq!(epoch_item.epoch(), 5);

        let simple_item = Fetch::new(100, true, 0);
        assert_eq!(*simple_item.data(), 100);
        assert!(simple_item.is_eof());

        let data = simple_item.into_data();
        assert_eq!(data, 100);
    }

    #[test]
    fn test_fetch_with_epoch() {
        // Verify Fetch works with epochs (for AsyncWorker)
        let epoch_item: Fetch<i32> = Fetch::new(42, false, 5);
        assert_eq!(epoch_item.data, 42);
        assert_eq!(epoch_item.epoch, 5);

        // Verify Fetch works with epoch=0 (for SyncWorker)
        let simple_item: Fetch<i32> = Fetch::new(100, true, 0);
        assert_eq!(simple_item.data, 100);
        assert_eq!(simple_item.epoch, 0);
    }

    #[test]
    fn test_into_inner_api() {
        let fetch = Fetch::new(vec![1u8, 2, 3], false, 0);
        let bytes = fetch.into_inner();
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn test_as_ref_api() {
        let fetch = Fetch::new(vec![1u8, 2, 3], false, 0);
        let bytes_ref: &Vec<u8> = fetch.as_ref();
        assert_eq!(bytes_ref, &vec![1, 2, 3]);
    }
}
