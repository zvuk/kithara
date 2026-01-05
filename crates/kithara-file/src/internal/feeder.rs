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
    chunk_size: usize,
}

impl Feeder {
    pub fn new(start_pos: u64, chunk_size: usize) -> Self {
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
                let end = pos.saturating_add(self.chunk_size as u64);

                match res.wait_range(pos..end).await? {
                    StorageWaitOutcome::Ready => {
                        let len = self.chunk_size;
                        let bytes = res.read_at(pos, len).await?;

                        trace!(pos, requested = len, got = bytes.len(), "kithara-file feeder: read_at");

                        if bytes.is_empty() {
                            // Contract check:
                            // `wait_range(..) -> Ready` means at least 1 byte in the requested range
                            // is readable. Empty here likely indicates a storage bug or misuse.
                            //
                            // async_stream's `stream!` macro can't `return Err(...)` because the
                            // stream item type is `Result<Bytes, StorageError>`. Instead, yield the
                            // error and terminate the stream.
                            yield Err(kithara_storage::StorageError::InvalidRange { start: pos, end });
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
