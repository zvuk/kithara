//! Test-only `Read + Seek` adapters used by the mp4 scanner unit tests
//! to assert that the scanner does not pull large box payloads
//! (`mdat`, cover art) into memory and does not seek to `SeekFrom::End`.

use std::io::{Cursor, Read, Seek, SeekFrom};

pub(super) struct CountingCursor {
    inner: Cursor<Vec<u8>>,
    pub(super) bytes_read: usize,
}

impl CountingCursor {
    pub(super) fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
            bytes_read: 0,
        }
    }
}

impl Read for CountingCursor {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.bytes_read += bytes_read;
        Ok(bytes_read)
    }
}

impl Seek for CountingCursor {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

pub(super) struct NoEndSeekCursor {
    inner: Cursor<Vec<u8>>,
}

impl NoEndSeekCursor {
    pub(super) fn new(data: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(data),
        }
    }
}

impl Read for NoEndSeekCursor {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for NoEndSeekCursor {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::End(_) => Err(std::io::Error::other("seek end forbidden")),
            _ => self.inner.seek(pos),
        }
    }
}
