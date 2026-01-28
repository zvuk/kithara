#![forbid(unsafe_code)]

use std::ops::Range;

use async_trait::async_trait;
use kithara_storage::WaitOutcome;

use crate::{error::StreamResult, media_info::MediaInfo};

/// Async random-access source contract.
///
/// Generic over `Item` type:
/// - `Source<Item=u8>` for byte sources (files, HTTP streams)
/// - `Source<Item=f32>` for PCM audio sources (decoded audio)
#[async_trait]
pub trait Source: Send + Sync + 'static {
    type Item: Send + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Wait for a range to be available.
    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error>;

    /// Read items at `offset` into `buf`. Returns number of items read.
    async fn read_at(
        &self,
        offset: u64,
        buf: &mut [Self::Item],
    ) -> StreamResult<usize, Self::Error>;

    /// Return known total length if available.
    fn len(&self) -> Option<u64>;

    fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}
