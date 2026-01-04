#![forbid(unsafe_code)]

use async_stream::stream;
use futures::Stream;
use kithara_assets::AssetResource;
use kithara_storage::{StreamingResource, StreamingResourceExt, WaitOutcome as StorageWaitOutcome};
use tracing::trace;

/// Shared "read loop" abstraction: `wait_range` → `read_at` → yield `Bytes`.
///
/// This is the streaming counterpart to `Fetcher`:
/// - `Fetcher` fills a `StreamingResource`
/// - `Feeder` reads it progressively and yields chunks
///
/// This module is internal to `kithara-file`.
#[derive(Clone, Copy, Debug)]
pub struct Feeder {
    start_pos: u64,
    chunk_size: u64,
}

impl Feeder {
    pub fn new(start_pos: u64, chunk_size: u64) -> Self {
        Self {
            start_pos,
            chunk_size,
        }
    }

    pub fn stream(
        self,
        res: AssetResource<
            StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    ) -> impl Stream<Item = Result<bytes::Bytes, kithara_storage::StorageError>> + Send + 'static
    {
        stream! {
            let mut pos = self.start_pos;

            loop {
                let end = pos.saturating_add(self.chunk_size);

                match res.wait_range(pos..end).await? {
                    StorageWaitOutcome::Ready => {
                        let len: usize = (end - pos) as usize;
                        let bytes = res.read_at(pos, len).await?;

                        trace!(pos, requested = len, got = bytes.len(), "kithara-file feeder: read_at");

                        if bytes.is_empty() {
                            // Defensive: after Ready we expect at least 1 byte; treat empty as EOF-ish.
                            return;
                        }

                        pos = pos.saturating_add(bytes.len() as u64);
                        yield Ok(bytes);
                    }
                    StorageWaitOutcome::Eof => {
                        trace!(pos, "kithara-file feeder: EOF");
                        return;
                    }
                }
            }
        }
    }
}
