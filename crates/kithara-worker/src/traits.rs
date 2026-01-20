//! Core worker traits for generic source abstraction.

use async_trait::async_trait;
#[cfg(any(test, feature = "test-utils"))]
use mockall::automock;

use crate::item::Fetch;

/// Base trait for all workers.
///
/// Provides a unified interface for running workers regardless of sync/async implementation.
#[async_trait]
#[cfg_attr(any(test, feature = "test-utils"), automock)]
pub trait Worker: Send + 'static {
    /// Run the worker until completion or error.
    ///
    /// This method should block (for sync workers) or be awaited (for async workers)
    /// until the worker completes.
    async fn run(self);
}

/// Trait for async sources that can be processed by a worker.
///
/// Implementors provide:
/// - `Chunk`: The type of data chunks to process (e.g., `Vec<u8>` for bytes, `PcmChunk<T>` for audio)
/// - `Command`: The type of commands (e.g., seek position)
/// - `fetch_next`: Async method to get the next chunk
/// - `handle_command`: Process a command (e.g., update position on seek)
/// - `eof_chunk`: Create a chunk to signal EOF
///
/// Note: Mockall cannot auto-generate mocks for traits with associated types.
/// Use manual mocking in tests for concrete types.
#[async_trait]
pub trait AsyncWorkerSource: Send + 'static {
    /// Type of data chunks produced.
    type Chunk: Send + 'static;

    /// Type of commands that can be sent to the source.
    type Command: Send + 'static;

    /// Fetch the next chunk of data.
    ///
    /// Returns a `Fetch` containing the chunk, EOF flag, and epoch.
    async fn fetch_next(&mut self) -> Fetch<Self::Chunk>;

    /// Handle a command (e.g., seek).
    ///
    /// Returns the new epoch after handling the command.
    fn handle_command(&mut self, cmd: Self::Command) -> u64;

    /// Get current epoch.
    fn epoch(&self) -> u64;
}

/// Trait for synchronous sources that can be processed by a blocking worker.
///
/// This is the blocking counterpart to `AsyncWorkerSource`, designed for sources
/// like audio decoders that have synchronous APIs.
///
/// Note: Mockall cannot auto-generate mocks for traits with associated types.
/// Use manual mocking in tests for concrete types.
pub trait SyncWorkerSource: Send + 'static {
    /// Type of data chunks produced.
    type Chunk: Send + 'static;

    /// Type of commands that can be sent to the source.
    type Command: Send + 'static;

    /// Fetch the next chunk of data (synchronous).
    ///
    /// Returns a `Fetch` containing the chunk and EOF flag.
    /// Epoch is always 0 for sync sources.
    fn fetch_next(&mut self) -> Fetch<Self::Chunk>;

    /// Handle a command.
    fn handle_command(&mut self, cmd: Self::Command);
}
