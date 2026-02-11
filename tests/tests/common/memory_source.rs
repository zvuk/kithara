//! Common in-memory Source implementation for testing.

use std::ops::Range;

use kithara_stream::{MediaInfo, Source, StreamResult, WaitOutcome};

/// Error type for memory-backed sources.
#[derive(Debug, thiserror::Error)]
#[error("memory source error")]
pub struct MemorySourceError;

/// In-memory source for testing Source-based readers.
pub struct MemorySource {
    data: Vec<u8>,
}

impl MemorySource {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn from_str(s: &str) -> Self {
        Self::new(s.as_bytes().to_vec())
    }
}

impl Source for MemorySource {
    type Item = u8;
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
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

impl Source for UnknownLenSource {
    type Item = u8;
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
