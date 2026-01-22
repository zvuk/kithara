//! ChunkedReader - continuous reader that accepts chunks and auto-releases read data.
//!
//! Designed for stream decoding where:
//! - Decoder needs continuous Read+Seek interface
//! - Data arrives in chunks (segments)
//! - Memory must be freed after reading
//!
//! ## Memory Management
//!
//! ChunkedReader maintains a sliding window:
//! - New chunks appended to the end
//! - Read data marked and can be released
//! - Only unread + small read buffer kept in memory
//!
//! ## Shared Access
//!
//! SharedChunkedReader wraps ChunkedReader in Arc<Mutex<>> for concurrent access:
//! - Decoder reads from it
//! - Decoder owner appends new chunks

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

use bytes::Bytes;

/// Chunked reader with automatic memory release.
///
/// Allows decoder to read continuously while freeing read data.
pub struct ChunkedReader {
    /// Chunks of data (not yet fully read)
    chunks: VecDeque<Bytes>,

    /// Current read position within current chunk
    chunk_offset: usize,

    /// Total bytes available
    total_len: usize,

    /// Global position (for seek)
    position: u64,

    /// How much data has been consumed and can be released
    consumed_bytes: usize,
}

impl ChunkedReader {
    /// Create new empty reader.
    pub fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            chunk_offset: 0,
            total_len: 0,
            position: 0,
            consumed_bytes: 0,
        }
    }

    /// Append new data chunk.
    ///
    /// This extends the readable range without requiring decoder recreation.
    pub fn append(&mut self, data: Bytes) {
        if data.is_empty() {
            return;
        }
        let data_len = data.len();
        self.total_len += data_len;
        self.chunks.push_back(data);

        // Debug memory usage (only when accumulating)
        if self.chunks.len() > 2 {
            tracing::warn!(
                chunks_count = self.chunks.len(),
                total_bytes = self.total_len,
                "ChunkedReader accumulating chunks (backpressure issue?)"
            );
        }
    }

    /// Clear all data and reset position.
    ///
    /// Used when decoder needs to be recreated (variant switch).
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.chunk_offset = 0;
        self.total_len = 0;
        self.position = 0;
        self.consumed_bytes = 0;
    }

    /// Release fully read chunks to free memory.
    ///
    /// Note: Chunks are automatically released in read() when exhausted.
    /// This method is kept for explicit memory management if needed.
    pub fn release_consumed(&mut self) {
        // No-op: chunks are already released in read() loop
        // consumed_bytes is just for tracking/stats
    }

    /// Get total available bytes.
    pub fn len(&self) -> usize {
        self.total_len
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }
}

impl Default for ChunkedReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.chunks.is_empty() {
            return Ok(0);
        }

        let mut total_read = 0;

        while total_read < buf.len() && !self.chunks.is_empty() {
            let chunk = &self.chunks[0];

            if self.chunk_offset >= chunk.len() {
                // Current chunk exhausted, remove it immediately to free memory
                let chunk_len = chunk.len();
                self.chunks.pop_front();
                self.chunk_offset = 0;
                self.consumed_bytes += chunk_len;

                tracing::trace!(
                    removed_chunk_bytes = chunk_len,
                    chunks_remaining = self.chunks.len(),
                    "ChunkedReader removed exhausted chunk"
                );

                continue;
            }

            let available = chunk.len() - self.chunk_offset;
            let to_read = (buf.len() - total_read).min(available);

            buf[total_read..total_read + to_read]
                .copy_from_slice(&chunk[self.chunk_offset..self.chunk_offset + to_read]);

            self.chunk_offset += to_read;
            total_read += to_read;
            self.position += to_read as u64;
        }

        Ok(total_read)
    }
}

impl Seek for ChunkedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.position as i64 + n,
            SeekFrom::End(n) => self.total_len as i64 + n,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }

        let new_pos = new_pos as u64;

        // Calculate which chunk and offset
        let mut acc = 0u64;
        let mut target_chunk_idx = 0;
        let mut target_offset = 0;

        for (idx, chunk) in self.chunks.iter().enumerate() {
            if new_pos < acc + chunk.len() as u64 {
                target_chunk_idx = idx;
                target_offset = (new_pos - acc) as usize;
                break;
            }
            acc += chunk.len() as u64;
        }

        // If seeking beyond available data, position at end
        if new_pos >= self.total_len as u64 {
            self.position = self.total_len as u64;
            // Position at end of last chunk
            if let Some(last) = self.chunks.back() {
                self.chunk_offset = last.len();
            }
            return Ok(self.position);
        }

        // Drop chunks before target
        for _ in 0..target_chunk_idx {
            if let Some(chunk) = self.chunks.pop_front() {
                self.consumed_bytes += chunk.len();
            }
        }

        self.chunk_offset = target_offset;
        self.position = new_pos;

        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_single_chunk() {
        let mut reader = ChunkedReader::new();
        reader.append(Bytes::from("hello"));

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();

        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn test_read_multiple_chunks() {
        let mut reader = ChunkedReader::new();
        reader.append(Bytes::from("hello"));
        reader.append(Bytes::from(" world"));

        let mut buf = [0u8; 20];
        let n = reader.read(&mut buf).unwrap();

        assert_eq!(n, 11);
        assert_eq!(&buf[..11], b"hello world");
    }

    #[test]
    fn test_seek() {
        let mut reader = ChunkedReader::new();
        reader.append(Bytes::from("hello world"));

        reader.seek(SeekFrom::Start(6)).unwrap();

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();

        assert_eq!(&buf[..n], b"world");
    }

    #[test]
    fn test_memory_release() {
        let mut reader = ChunkedReader::new();
        reader.append(Bytes::from("chunk1"));
        reader.append(Bytes::from("chunk2"));

        // Read first chunk completely
        let mut buf = [0u8; 6];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"chunk1");

        // First chunk exhausted but still in queue (offset at end)
        assert_eq!(reader.chunks.len(), 2);
        assert_eq!(reader.chunk_offset, 6);

        // Read next byte - this will trigger move to next chunk and release first
        let mut buf2 = [0u8; 1];
        let n = reader.read(&mut buf2).unwrap();
        assert_eq!(n, 1);
        assert_eq!(&buf2, b"c"); // First byte of "chunk2"

        // Now first chunk should be released
        assert_eq!(reader.chunks.len(), 1);
        assert_eq!(reader.consumed_bytes, 6);
    }
}

/// Shared chunked reader for concurrent access.
///
/// Wraps ChunkedReader in Arc<Mutex<>> so decoder can read while
/// owner appends new chunks.
#[derive(Clone)]
pub struct SharedChunkedReader {
    inner: Arc<Mutex<ChunkedReader>>,
}

impl SharedChunkedReader {
    /// Create new shared reader.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ChunkedReader::new())),
        }
    }

    /// Append new data chunk.
    pub fn append(&self, data: Bytes) {
        self.inner.lock().unwrap().append(data);
    }

    /// Clear all data.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// Release consumed data.
    pub fn release_consumed(&self) {
        self.inner.lock().unwrap().release_consumed();
    }
}

impl Default for SharedChunkedReader {
    fn default() -> Self {
        Self::new()
    }
}

impl Read for SharedChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.lock().unwrap().read(buf)
    }
}

impl Seek for SharedChunkedReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.lock().unwrap().seek(pos)
    }
}
