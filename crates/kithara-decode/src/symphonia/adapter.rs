//! `MediaSource` adapter bridging a `Read + Seek` source to Symphonia.
//!
//! Tracks the byte length dynamically via an `Arc<AtomicU64>` so that HLS
//! flows can update the reported length after construction, and exposes a
//! seek-enable toggle so fMP4 reader initialization can temporarily disable
//! seeking.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use symphonia::core::io::MediaSource;

/// Adapter that wraps a Read + Seek source as a Symphonia [`MediaSource`].
pub(crate) struct ReadSeekAdapter<R> {
    /// Dynamic byte length. Updated externally via `Arc<AtomicU64>`.
    /// 0 means unknown (returns None from `byte_len()`).
    byte_len: Arc<AtomicU64>,
    /// Live read-cursor position. Updated on every `Read::read` /
    /// `Seek::seek` so the decoder layer can read it after
    /// `format_reader.seek()` to learn the absolute byte offset
    /// where the next packet body will be read from. The pipeline
    /// uses this to plug a real byte target into `Stream::seek`,
    /// avoiding ad-hoc `frame × bytes_per_frame` recomputation.
    byte_pos: Arc<AtomicU64>,
    /// Controls whether seek operations are allowed.
    /// Used to temporarily disable seeking during fMP4 reader initialization
    /// (prevents `IsoMp4Reader` from seeking to end looking for moov atom).
    seek_enabled: Arc<AtomicBool>,
    inner: R,
}

impl<R: Seek> ReadSeekAdapter<R> {
    pub(crate) fn byte_len_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_len)
    }

    pub(crate) fn byte_pos_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_pos)
    }

    pub(crate) fn new_inner(
        mut inner: R,
        shared_handle: Option<Arc<AtomicU64>>,
        seek_enabled: bool,
    ) -> Self {
        let has_shared_value = shared_handle
            .as_ref()
            .is_some_and(|h| h.load(Ordering::Acquire) > 0);
        let byte_len = shared_handle.unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        let seek_enabled = Arc::new(AtomicBool::new(seek_enabled));
        // Probe byte length from the source. Skip when a shared handle
        // already carries a non-zero value (e.g. Content-Length from HTTP) —
        // streaming sources may have only a fraction downloaded, so
        // seek(End(0)) would return the buffered size, not the total.
        if !has_shared_value && let Some(len) = Self::probe_byte_len(&mut inner) {
            byte_len.store(len, Ordering::Release);
        }
        let initial_pos = inner.stream_position().unwrap_or(0);
        Self {
            byte_len,
            seek_enabled,
            inner,
            byte_pos: Arc::new(AtomicU64::new(initial_pos)),
        }
    }

    /// Create adapter with seek initially disabled.
    pub(crate) fn new_seek_disabled(inner: R) -> Self {
        Self::new_inner(inner, None, false)
    }

    /// Create adapter with a shared byte-length handle and seek disabled.
    pub(crate) fn new_seek_disabled_shared(inner: R, handle: Arc<AtomicU64>) -> Self {
        Self::new_inner(inner, Some(handle), false)
    }

    /// Create adapter with seek enabled from the start.
    pub(crate) fn new_seek_enabled(inner: R) -> Self {
        Self::new_inner(inner, None, true)
    }

    fn probe_byte_len(reader: &mut R) -> Option<u64> {
        let current = reader.stream_position().ok()?;
        let end = reader.seek(SeekFrom::End(0)).ok()?;
        reader.seek(SeekFrom::Start(current)).ok()?;
        Some(end)
    }

    pub(crate) fn seek_enabled_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.seek_enabled)
    }
}

impl<R: Read> Read for ReadSeekAdapter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.byte_pos.fetch_add(n as u64, Ordering::Release);
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for ReadSeekAdapter<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = self.inner.seek(pos)?;
        self.byte_pos.store(new_pos, Ordering::Release);
        Ok(new_pos)
    }
}

impl<R: Read + Seek + Send + Sync> MediaSource for ReadSeekAdapter<R> {
    fn byte_len(&self) -> Option<u64> {
        let len = self.byte_len.load(Ordering::Acquire);
        if len > 0 { Some(len) } else { None }
    }

    fn is_seekable(&self) -> bool {
        self.seek_enabled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kithara_test_utils::kithara;
    use symphonia::core::io::MediaSource;

    use super::*;

    #[kithara::test]
    fn test_read_seek_adapter_byte_len() {
        let data = vec![0u8; 5000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new_seek_disabled(cursor);

        assert_eq!(adapter.byte_len(), Some(5000));
        assert!(!adapter.is_seekable());

        adapter.seek_enabled_handle().store(true, Ordering::Release);
        assert!(adapter.is_seekable());
    }

    #[kithara::test]
    fn test_read_seek_adapter_dynamic_update() {
        let data = vec![0u8; 1000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new_seek_disabled(cursor);
        let handle = adapter.byte_len_handle();

        handle.store(0, Ordering::Release);
        assert_eq!(adapter.byte_len(), None);

        handle.store(2000, Ordering::Release);
        assert_eq!(adapter.byte_len(), Some(2000));
    }
}
