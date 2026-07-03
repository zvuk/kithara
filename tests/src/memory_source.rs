use std::{
    io::Error as IoError,
    num::NonZeroUsize,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::executor::block_on;
use kithara::{
    events::EventBus,
    platform::time::Duration,
    storage::WaitOutcome,
    stream::{
        Activity, PlayheadRead, PlayheadState, PlayheadWrite, ReadOutcome, SeekControl,
        SeekObserve, SeekState, Source, SourceError, SourcePhase, Stream, StreamResult, StreamType,
    },
};

/// Error type for memory-backed sources.
#[derive(Debug, thiserror::Error)]
#[error("memory source error")]
pub struct MemorySourceError;

/// In-memory source for testing Source-based readers.
///
/// When `report_len` is `true` (default), `len()` returns `Some(data.len())`.
/// Set `report_len` to `false` to simulate sources without known length
/// (e.g. for testing `SeekFrom::End` error paths).
pub struct MemorySource {
    seek: Arc<SeekState>,
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    data: Vec<u8>,
    report_len: bool,
}

impl MemorySource {
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            report_len: true,
        }
    }

    /// Create a source that reports unknown length (`len()` returns `None`).
    #[must_use]
    pub fn without_len(data: Vec<u8>) -> Self {
        Self {
            data,
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            report_len: false,
        }
    }
}

impl Source for MemorySource {
    fn len(&self) -> Option<u64> {
        if self.report_len {
            Some(self.data.len() as u64)
        } else {
            None
        }
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if range.start >= self.data.len() as u64 {
            SourcePhase::Eof
        } else {
            SourcePhase::Ready
        }
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(ReadOutcome::Eof);
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(ReadOutcome::Bytes(count))
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek) as Arc<dyn SeekObserve>
    }

    fn seek_control(&self) -> Arc<dyn SeekControl> {
        Arc::clone(&self.seek) as Arc<dyn SeekControl>
    }

    fn activity(&self) -> Arc<dyn Activity> {
        Arc::clone(&self.seek) as Arc<dyn Activity>
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let _ = timeout;
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }
}

/// Backwards-compatible alias for `MemorySource::without_len`.
pub type UnknownLenSource = MemorySource;

/// `StreamType` using `MemorySource` (known length).
pub struct MemStream;

impl StreamType for MemStream {
    type Config = MemStreamConfig;
    type Events = EventBus;
    type Source = MemorySource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        config
            .source
            .ok_or_else(|| SourceError::other(IoError::other("no source")))
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.event_bus.clone()
    }
}

#[derive(Default)]
pub struct MemStreamConfig {
    pub source: Option<MemorySource>,
    pub event_bus: Option<EventBus>,
}

/// `StreamType` using `MemorySource` with unknown length.
pub struct UnknownLenStream;

impl StreamType for UnknownLenStream {
    type Config = UnknownLenStreamConfig;
    type Events = EventBus;
    type Source = MemorySource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        config
            .source
            .ok_or_else(|| SourceError::other(IoError::other("no source")))
    }

    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        config.event_bus.clone()
    }
}

#[derive(Default)]
pub struct UnknownLenStreamConfig {
    pub source: Option<MemorySource>,
    pub event_bus: Option<EventBus>,
}

/// Create a `Stream` from a `MemorySource`.
#[must_use]
pub fn memory_stream(source: MemorySource) -> Stream<MemStream> {
    let config = MemStreamConfig {
        source: Some(source),
        event_bus: None,
    };
    block_on(Stream::new(config)).unwrap()
}

/// Create a `Stream` from a `MemorySource` with unknown length.
#[must_use]
pub fn unknown_len_stream(source: MemorySource) -> Stream<UnknownLenStream> {
    let config = UnknownLenStreamConfig {
        source: Some(source),
        event_bus: None,
    };
    block_on(Stream::new(config)).unwrap()
}
