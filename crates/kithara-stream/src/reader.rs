#![forbid(unsafe_code)]

//! Sync reader with random access.
//!
//! `Reader<S>` implements `Read + Seek` by calling Source directly.
//! No channels, no `block_on` — Source methods are sync.

use std::{
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_storage::WaitOutcome;

use crate::{MediaInfo, source::Source};

/// Sync reader with random access.
///
/// Calls `Source` methods directly for I/O.
/// Implements `Read + Seek` for use with symphonia.
pub struct Reader<S: Source> {
    pos: Arc<AtomicU64>,
    source: S,
}

impl<S: Source> Reader<S> {
    /// Create new reader.
    pub fn new(source: S) -> Self {
        Self {
            pos: Arc::new(AtomicU64::new(0)),
            source,
        }
    }

    /// Get handle to shared byte position (for `StreamContext`).
    pub fn position_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.pos)
    }

    /// Get current position.
    pub fn position(&self) -> u64 {
        self.pos.load(Ordering::Relaxed)
    }

    /// Get total length if known.
    pub fn len(&self) -> Option<u64> {
        self.source.len()
    }

    /// Check if length is zero or unknown.
    pub fn is_empty(&self) -> bool {
        self.source.len().is_none_or(|l| l == 0)
    }

    /// Get media info if known.
    pub fn media_info(&self) -> Option<MediaInfo> {
        self.source.media_info()
    }

    /// Get current segment byte range (for segmented sources like HLS).
    pub fn current_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.source.current_segment_range()
    }

    /// Get byte range of first segment with current format after ABR switch.
    pub fn format_change_segment_range(&self) -> Option<std::ops::Range<u64>> {
        self.source.format_change_segment_range()
    }

    /// Clear variant fence, allowing reads from the next variant.
    pub fn clear_variant_fence(&mut self) {
        self.source.clear_variant_fence();
    }

    /// Get mutable reference to inner source.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// Get shared reference to inner source.
    pub fn source(&self) -> &S {
        &self.source
    }
}

impl<S: Source> Read for Reader<S> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let pos = self.pos.load(Ordering::Relaxed);
        let range = pos..pos.saturating_add(buf.len() as u64);

        // Wait for data to be available (blocking)
        match self
            .source
            .wait_range(range)
            .map_err(|e| std::io::Error::other(e.to_string()))?
        {
            WaitOutcome::Ready => {}
            WaitOutcome::Eof => return Ok(0),
        }

        // Read data directly from source
        let n = self
            .source
            .read_at(pos, buf)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        self.pos
            .store(pos.saturating_add(n as u64), Ordering::Relaxed);
        Ok(n)
    }
}

impl<S: Source> Seek for Reader<S> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let current = self.pos.load(Ordering::Relaxed);
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (current as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        let new_pos = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "seek past EOF: new_pos={new_pos} len={len} current_pos={current} seek_from={pos:?}",
                ),
            ));
        }

        self.pos.store(new_pos, Ordering::Relaxed);
        Ok(new_pos)
    }
}
