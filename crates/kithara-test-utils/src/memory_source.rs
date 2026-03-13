//! In-memory Source implementation for testing.

use std::{io, ops::Range};

use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    DemandSlot, ReadOutcome, Source, SourcePhase, Stream, StreamResult, StreamType, Timeline,
    TransferCoordination,
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
    coord: MemoryCoord,
    data: Vec<u8>,
    report_len: bool,
}

impl MemorySource {
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            coord: MemoryCoord::default(),
            data,
            report_len: true,
        }
    }

    /// Create a source that reports unknown length (`len()` returns `None`).
    #[must_use]
    pub fn without_len(data: Vec<u8>) -> Self {
        Self {
            coord: MemoryCoord::default(),
            data,
            report_len: false,
        }
    }
}

impl Source for MemorySource {
    type Error = MemorySourceError;
    type Topology = ();
    type Layout = ();
    type Coord = MemoryCoord;
    type Demand = ();

    fn topology(&self) -> &Self::Topology {
        &()
    }

    fn layout(&self) -> &Self::Layout {
        &()
    }

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        let _ = timeout;
        if range.start >= self.data.len() as u64 {
            Ok(WaitOutcome::Eof)
        } else {
            Ok(WaitOutcome::Ready)
        }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let offset = offset as usize;
        if offset >= self.data.len() {
            return Ok(ReadOutcome::Data(0));
        }
        let available = self.data.len() - offset;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[offset..offset + n]);
        Ok(ReadOutcome::Data(n))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if range.start >= self.data.len() as u64 {
            SourcePhase::Eof
        } else {
            SourcePhase::Ready
        }
    }

    fn len(&self) -> Option<u64> {
        if self.report_len {
            Some(self.data.len() as u64)
        } else {
            None
        }
    }
}

/// Backwards-compatible alias for `MemorySource::without_len`.
pub type UnknownLenSource = MemorySource;

// StreamType markers for testing Read+Seek behavior with Stream<T>.

pub struct MemoryCoord {
    demand: DemandSlot<()>,
    timeline: Timeline,
}

impl Default for MemoryCoord {
    fn default() -> Self {
        Self {
            demand: DemandSlot::new(),
            timeline: Timeline::new(),
        }
    }
}

impl TransferCoordination<()> for MemoryCoord {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn demand(&self) -> &DemandSlot<()> {
        &self.demand
    }
}

/// `StreamType` using `MemorySource` (known length).
pub struct MemStream;

impl StreamType for MemStream {
    type Config = MemStreamConfig;
    type Coord = MemoryCoord;
    type Demand = ();
    type Source = MemorySource;
    type Error = io::Error;
    type Events = ();
    type Layout = ();
    type Topology = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config.source.ok_or_else(|| io::Error::other("no source"))
    }
}

#[derive(Default)]
pub struct MemStreamConfig {
    pub source: Option<MemorySource>,
}

/// `StreamType` using `MemorySource` with unknown length.
pub struct UnknownLenStream;

impl StreamType for UnknownLenStream {
    type Config = UnknownLenStreamConfig;
    type Coord = MemoryCoord;
    type Demand = ();
    type Source = MemorySource;
    type Error = io::Error;
    type Events = ();
    type Layout = ();
    type Topology = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config.source.ok_or_else(|| io::Error::other("no source"))
    }
}

#[derive(Default)]
pub struct UnknownLenStreamConfig {
    pub source: Option<MemorySource>,
}

/// Create a `Stream` from a `MemorySource`.
#[must_use]
pub fn memory_stream(source: MemorySource) -> Stream<MemStream> {
    let config = MemStreamConfig {
        source: Some(source),
    };
    futures::executor::block_on(Stream::new(config)).unwrap()
}

/// Create a `Stream` from a `MemorySource` with unknown length.
#[must_use]
pub fn unknown_len_stream(source: MemorySource) -> Stream<UnknownLenStream> {
    let config = UnknownLenStreamConfig {
        source: Some(source),
    };
    futures::executor::block_on(Stream::new(config)).unwrap()
}
