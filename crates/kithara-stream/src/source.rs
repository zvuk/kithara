#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
};

use async_trait::async_trait;
use kithara_storage::WaitOutcome;
use tracing::trace;

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
    async fn read_at(&self, offset: u64, buf: &mut [Self::Item])
        -> StreamResult<usize, Self::Error>;

    /// Return known total length if available.
    fn len(&self) -> Option<u64>;

    fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        None
    }
}

/// Sync source trait for blocking read operations.
///
/// This is the simple interface that `SyncReader` uses.
/// Implementations can read from disk, memory, or anywhere else.
pub trait SyncSource: Send + 'static {
    /// Wait for range to be available (blocking).
    fn wait_range(&self, range: Range<u64>) -> std::io::Result<WaitOutcome>;

    /// Read bytes at offset (blocking). Returns number of bytes read.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;
}

/// Simple sync `Read + Seek` adapter.
///
/// No prefetching, no channels, no workers, no buffer pools.
/// Just waits for data and reads it.
pub struct SyncReader<S: SyncSource> {
    source: S,
    pos: u64,
    eof: bool,
}

impl<S: SyncSource> SyncReader<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            pos: 0,
            eof: false,
        }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_inner(self) -> S {
        self.source
    }
}

impl<S: SyncSource> Read for SyncReader<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.eof {
            trace!(
                buf_empty = buf.is_empty(),
                eof = self.eof,
                pos = self.pos,
                "SyncReader::read early return"
            );
            return Ok(0);
        }

        let range = self.pos..self.pos.saturating_add(buf.len() as u64);
        trace!(
            start = range.start,
            end = range.end,
            buf_len = buf.len(),
            "SyncReader::read wait_range"
        );

        match self.source.wait_range(range)? {
            WaitOutcome::Ready => {}
            WaitOutcome::Eof => {
                eprintln!("[DEBUG] SyncReader::read wait_range returned Eof at pos={}", self.pos);
                self.eof = true;
                return Ok(0);
            }
        }

        let n = self.source.read_at(self.pos, buf)?;
        if n == 0 {
            eprintln!("[DEBUG] SyncReader::read read_at returned 0 at pos={}", self.pos);
            self.eof = true;
        } else {
            self.pos = self.pos.saturating_add(n as u64);
            trace!(bytes = n, new_pos = self.pos, "SyncReader::read");
        }
        Ok(n)
    }
}

impl<S: SyncSource> Seek for SyncReader<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        trace!(?pos, current_pos = self.pos, "SyncReader::seek begin");

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    trace!("SyncReader::seek End failed - unknown length");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "unknown length",
                    ));
                };
                trace!(len, delta, "SyncReader::seek End with known length");
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            trace!(new_pos, "SyncReader::seek negative position");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek",
            ));
        }

        let new_pos_u64 = new_pos as u64;

        if let Some(len) = self.source.len() {
            if new_pos_u64 > len {
                trace!(new_pos_u64, len, "SyncReader::seek past EOF");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "seek past EOF",
                ));
            }
        }

        self.pos = new_pos_u64;
        self.eof = false;
        trace!(new_pos = self.pos, "SyncReader::seek done");
        Ok(self.pos)
    }
}
