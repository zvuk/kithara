//! Mock implementations for testing workers.
//!
//! ## Why manual mocks instead of mockall?
//!
//! We cannot use `#[automock]` for `AsyncWorkerSource` and `SyncWorkerSource`
//! because **mockall does not support traits with associated types**.
//!
//! Both traits have:
//! - `type Chunk: Send + 'static`
//! - `type Command: Send + 'static`
//!
//! Mockall requires knowing concrete types at compile time to generate mocks,
//! but generic associated types cannot be resolved until trait implementation.
//!
//! This is a fundamental limitation documented in:
//! <https://docs.rs/mockall/latest/mockall/#associated-types>
//!
//! ## Our approach
//!
//! Manual mocks with concrete types:
//! - `MockAsyncSource`: `Chunk = Vec<u8>`, `Command = SeekCommand`
//! - `MockSyncSource`: `Chunk = Vec<u8>`, `Command = ()`
//!
//! This gives us:
//! - Full control over behavior
//! - Simple, readable implementation
//! - No macro complexity
//! - Easy to extend for new test scenarios

use async_trait::async_trait;

use crate::{
    item::Fetch,
    traits::{AsyncWorkerSource, SyncWorkerSource},
};

/// Simple seek command with offset and epoch.
#[derive(Debug, Clone, Copy)]
pub struct SeekCommand {
    pub offset: usize,
    pub epoch: u64,
}

/// Mock async source for testing AsyncWorker.
///
/// Generates `Vec<u8>` chunks where each chunk is filled with its index.
/// Supports seek commands for epoch invalidation testing.
#[derive(Debug)]
pub struct MockAsyncSource {
    chunk_size: usize,
    total_chunks: usize,
    current: usize,
    epoch: u64,
}

impl MockAsyncSource {
    /// Create a new mock source.
    ///
    /// # Arguments
    /// * `chunk_size` - Size of each chunk in bytes
    /// * `total_chunks` - Total number of chunks before EOF
    pub fn new(chunk_size: usize, total_chunks: usize) -> Self {
        Self {
            chunk_size,
            total_chunks,
            current: 0,
            epoch: 0,
        }
    }
}

#[async_trait]
impl AsyncWorkerSource for MockAsyncSource {
    type Chunk = Vec<u8>;
    type Command = SeekCommand;

    async fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        // Check if we're at EOF
        if self.current >= self.total_chunks {
            return Fetch::new(Vec::new(), true, self.epoch);
        }

        // Generate a chunk filled with current index as u8
        let data = vec![self.current as u8; self.chunk_size];
        self.current += 1;

        Fetch::new(data, false, self.epoch)
    }

    fn handle_command(&mut self, cmd: Self::Command) -> u64 {
        self.current = cmd.offset;
        self.epoch = cmd.epoch;
        self.epoch
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Mock sync source for testing SyncWorker.
///
/// Provides a fixed set of `Vec<u8>` chunks.
#[derive(Debug)]
pub struct MockSyncSource {
    items: Vec<Vec<u8>>,
    index: usize,
}

impl MockSyncSource {
    /// Create a new mock sync source.
    ///
    /// # Arguments
    /// * `items` - Fixed list of chunks to return
    pub fn new(items: Vec<Vec<u8>>) -> Self {
        Self { items, index: 0 }
    }

    /// Create a mock source with N chunks of given size.
    pub fn with_count(chunk_size: usize, count: usize) -> Self {
        let items = (0..count).map(|i| vec![i as u8; chunk_size]).collect();
        Self::new(items)
    }
}

impl SyncWorkerSource for MockSyncSource {
    type Chunk = Vec<u8>;
    type Command = ();

    fn fetch_next(&mut self) -> Fetch<Self::Chunk> {
        // Check if we're at EOF
        if self.index >= self.items.len() {
            return Fetch::new(Vec::new(), true, 0);
        }

        let data = self.items[self.index].clone();
        self.index += 1;

        Fetch::new(data, false, 0)
    }

    fn handle_command(&mut self, _cmd: Self::Command) {
        // No-op for sync sources (no seek support in this mock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_async_source_basic() {
        let mut source = MockAsyncSource::new(1024, 3);

        // Fetch first chunk
        let fetch1 = source.fetch_next().await;
        assert!(!fetch1.is_eof());
        assert_eq!(fetch1.data.len(), 1024);
        assert_eq!(fetch1.data[0], 0); // First chunk filled with 0
        assert_eq!(fetch1.epoch, 0);

        // Fetch second chunk
        let fetch2 = source.fetch_next().await;
        assert!(!fetch2.is_eof());
        assert_eq!(fetch2.data[0], 1); // Second chunk filled with 1

        // Fetch third chunk
        let fetch3 = source.fetch_next().await;
        assert!(!fetch3.is_eof());
        assert_eq!(fetch3.data[0], 2); // Third chunk filled with 2

        // Fetch EOF
        let fetch_eof = source.fetch_next().await;
        assert!(fetch_eof.is_eof());
        assert!(fetch_eof.data.is_empty());
    }

    #[tokio::test]
    async fn test_mock_async_source_seek() {
        let mut source = MockAsyncSource::new(1024, 10);

        // Fetch first chunk
        let fetch1 = source.fetch_next().await;
        assert_eq!(fetch1.epoch, 0);

        // Seek to position 5 with new epoch
        source.handle_command(SeekCommand {
            offset: 5,
            epoch: 1,
        });

        // Next fetch should have new epoch
        let fetch2 = source.fetch_next().await;
        assert_eq!(fetch2.epoch, 1);
        assert_eq!(fetch2.data[0], 5); // Should be at offset 5
    }

    #[test]
    fn test_mock_sync_source_basic() {
        let mut source = MockSyncSource::with_count(512, 3);

        // Fetch all chunks
        for i in 0..3 {
            let fetch = source.fetch_next();
            assert!(!fetch.is_eof());
            assert_eq!(fetch.data.len(), 512);
            assert_eq!(fetch.data[0], i as u8);
        }

        // Fetch EOF
        let fetch_eof = source.fetch_next();
        assert!(fetch_eof.is_eof());
        assert!(fetch_eof.data.is_empty());
    }
}
