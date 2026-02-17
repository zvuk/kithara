//! In-memory Source implementation for testing.

use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicU64},
};

use kithara_storage::WaitOutcome;
use kithara_stream::{
    MediaInfo, NullStreamContext, Source, Stream, StreamContext, StreamResult, StreamType,
};

/// Error type for memory-backed sources.
#[derive(Debug, thiserror::Error)]
#[error("memory source error")]
pub struct MemorySourceError;

/// In-memory source for testing Source-based readers.
pub struct MemorySource {
    data: Vec<u8>,
}

impl MemorySource {
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl Source for MemorySource {
    type Error = MemorySourceError;

    fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}

/// Source without known length for testing `SeekFrom::End` error.
pub struct UnknownLenSource {
    data: Vec<u8>,
}

impl UnknownLenSource {
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl Source for UnknownLenSource {
    type Error = MemorySourceError;

    fn wait_range(&mut self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        None
    }
}

// StreamType markers for testing Read+Seek behavior with Stream<T>.

/// `StreamType` using `MemorySource` (known length).
pub struct MemStream;

impl StreamType for MemStream {
    type Config = MemStreamConfig;
    type Source = MemorySource;
    type Error = std::io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| std::io::Error::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(position))
    }
}

#[derive(Default)]
pub struct MemStreamConfig {
    pub source: Option<MemorySource>,
}

/// `StreamType` using `UnknownLenSource` (unknown length).
pub struct UnknownLenStream;

impl StreamType for UnknownLenStream {
    type Config = UnknownLenStreamConfig;
    type Source = UnknownLenSource;
    type Error = std::io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| std::io::Error::other("no source"))
    }

    fn build_stream_context(
        _source: &Self::Source,
        position: Arc<AtomicU64>,
    ) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(position))
    }
}

#[derive(Default)]
pub struct UnknownLenStreamConfig {
    pub source: Option<UnknownLenSource>,
}

/// Create a `Stream` from a `MemorySource`.
#[must_use]
pub fn memory_stream(source: MemorySource) -> Stream<MemStream> {
    let config = MemStreamConfig {
        source: Some(source),
    };
    futures::executor::block_on(Stream::new(config)).unwrap()
}

/// Create a `Stream` from an `UnknownLenSource`.
#[must_use]
pub fn unknown_len_stream(source: UnknownLenSource) -> Stream<UnknownLenStream> {
    let config = UnknownLenStreamConfig {
        source: Some(source),
    };
    futures::executor::block_on(Stream::new(config)).unwrap()
}
