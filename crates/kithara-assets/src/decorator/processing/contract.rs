use std::fmt::Debug;

use kithara_platform::sync::Arc;

/// Creates independent processing state for each resource commit.
pub trait ResourceProcessor: Send + Sync + Debug {
    /// Creates mutable processing state for one commit.
    fn begin(&self) -> Box<dyn ChunkSink>;

    /// Returns the exact byte identity used by the cache key.
    fn identity(&self) -> &[u8];
}

/// Transforms a resource one chunk at a time during commit.
pub trait ChunkSink: Send {
    /// Transforms `input` into `output` and returns the bytes written.
    ///
    /// # Errors
    /// Returns a message when the transform fails.
    fn process(&mut self, input: &[u8], output: &mut [u8], is_last: bool) -> Result<usize, String>;
}

/// Shared processing configuration for one acquisition.
pub type ProcessCtx = Arc<dyn ResourceProcessor>;
