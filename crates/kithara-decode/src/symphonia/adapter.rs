use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_platform::sync::Arc;
use symphonia_core::io::MediaSource;

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
    range: Option<Range<u64>>,
}

impl<R: Seek> ReadSeekAdapter<R> {
    /// Build the adapter. `shared_handle` is `Some` to publish/read the
    /// byte length through an externally owned cell instead of a fresh
    /// one; `seek_enabled` starts the adapter seekable or not (toggled
    /// later via `seek_enabled_handle`).
    pub(crate) fn new(
        mut inner: R,
        shared_handle: Option<Arc<AtomicU64>>,
        seek_enabled: bool,
    ) -> Self {
        let has_shared_handle = shared_handle.is_some();
        let byte_len = shared_handle.unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        let seek_enabled = Arc::new(AtomicBool::new(seek_enabled));
        if !has_shared_handle && let Some(len) = Self::probe_byte_len(&mut inner) {
            byte_len.store(len, Ordering::Release);
        }
        let initial_pos = inner.stream_position().unwrap_or(0);
        Self {
            byte_len,
            seek_enabled,
            inner,
            byte_pos: Arc::new(AtomicU64::new(initial_pos)),
            range: None,
        }
    }

    pub(crate) fn with_range(mut self, range: Range<u64>) -> io::Result<Self> {
        if range.start > range.end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid input range",
            ));
        }
        self.inner.seek(SeekFrom::Start(range.start))?;
        self.byte_pos.store(range.start, Ordering::Release);
        self.range = Some(range);
        Ok(self)
    }

    pub(crate) fn byte_len_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_len)
    }

    pub(crate) fn byte_pos_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.byte_pos)
    }

    fn probe_byte_len(reader: &mut R) -> Option<u64> {
        let current = reader.stream_position().ok()?;
        let end = reader.seek(SeekFrom::End(0)).ok()?;
        reader.seek(SeekFrom::Start(current)).ok()?;
        Some(end)
    }

    #[cfg(feature = "symphonia")]
    pub(crate) fn seek_enabled_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.seek_enabled)
    }
}

impl<R: Read> Read for ReadSeekAdapter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.range.as_ref().map_or(buf.len(), |range| {
            usize::try_from(
                range
                    .end
                    .saturating_sub(self.byte_pos.load(Ordering::Acquire)),
            )
            .unwrap_or(usize::MAX)
            .min(buf.len())
        });
        let buf = &mut buf[..available];
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.byte_pos.fetch_add(n as u64, Ordering::Release);
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for ReadSeekAdapter<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if let Some(range) = &self.range {
            let target = match pos {
                SeekFrom::Start(offset) => range.start.checked_add(offset),
                SeekFrom::End(offset) => range.end.checked_add_signed(offset),
                SeekFrom::Current(offset) => self
                    .byte_pos
                    .load(Ordering::Acquire)
                    .checked_add_signed(offset),
            }
            .filter(|target| range.contains(target) || *target == range.end)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "seek outside input range")
            })?;
            let physical = self.inner.seek(SeekFrom::Start(target))?;
            self.byte_pos.store(physical, Ordering::Release);
            return Ok(physical - range.start);
        }
        let new_pos = self.inner.seek(pos)?;
        self.byte_pos.store(new_pos, Ordering::Release);
        Ok(new_pos)
    }
}

impl<R: Read + Seek + Send + Sync> MediaSource for ReadSeekAdapter<R> {
    fn byte_len(&self) -> Option<u64> {
        if let Some(range) = &self.range {
            return Some(range.end - range.start);
        }
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
    use symphonia_core::io::MediaSource;

    use super::*;

    #[kithara::test]
    fn ranged_input_keeps_seek_and_read_evidence_in_source_coordinates() {
        let mut adapter = ReadSeekAdapter::new(Cursor::new(vec![9, 8, 1, 2, 3, 7]), None, true)
            .with_range(2..5)
            .expect("bounded encoded payload");
        assert_eq!(adapter.byte_len(), Some(3));
        let position = adapter.byte_pos_handle();
        let mut bytes = [0; 8];
        assert_eq!(adapter.read(&mut bytes).expect("read payload"), 3);
        assert_eq!(&bytes[..3], &[1, 2, 3]);
        assert_eq!(position.load(Ordering::Acquire), 5);
        assert_eq!(adapter.read(&mut bytes).expect("payload EOF"), 0);
        assert_eq!(adapter.seek(SeekFrom::Start(1)).expect("logical seek"), 1);
        assert_eq!(position.load(Ordering::Acquire), 3);
        assert_eq!(adapter.read(&mut bytes).expect("read suffix"), 2);
        assert_eq!(&bytes[..2], &[2, 3]);
        assert!(adapter.seek(SeekFrom::End(1)).is_err());
    }

    #[kithara::test]
    fn test_read_seek_adapter_byte_len() {
        let data = vec![0u8; 5000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new(cursor, None, false);

        assert_eq!(adapter.byte_len(), Some(5000));
        assert!(!adapter.is_seekable());
    }

    #[cfg(feature = "symphonia")]
    #[kithara::test]
    fn seek_enabled_handle_turns_seeking_on() {
        let adapter = ReadSeekAdapter::new(Cursor::new(vec![0u8; 5000]), None, false);

        adapter.seek_enabled_handle().store(true, Ordering::Release);
        assert!(adapter.is_seekable());
    }

    #[kithara::test]
    fn test_read_seek_adapter_dynamic_update() {
        let data = vec![0u8; 1000];
        let cursor = Cursor::new(data);
        let adapter = ReadSeekAdapter::new(cursor, None, false);
        let handle = adapter.byte_len_handle();

        handle.store(0, Ordering::Release);
        assert_eq!(adapter.byte_len(), None);

        handle.store(2000, Ordering::Release);
        assert_eq!(adapter.byte_len(), Some(2000));
    }

    #[kithara::test]
    fn published_unknown_length_stays_owned_by_the_stream() {
        let length = Arc::new(AtomicU64::new(0));
        let adapter =
            ReadSeekAdapter::new(Cursor::new(vec![0u8; 1_000]), Some(length.clone()), false);
        assert_eq!(adapter.byte_len(), None);
        assert_eq!(length.load(Ordering::Acquire), 0);
        length.store(2_000, Ordering::Release);
        assert_eq!(adapter.byte_len(), Some(2_000));
    }
}
