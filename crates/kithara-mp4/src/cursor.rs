use std::io::{self, Error, Read, Seek, SeekFrom};

use crate::consts;

/// Random-access byte source the index walk pulls box headers from.
pub trait ReadAt {
    /// Read into `buf` starting at `offset` and report how many bytes landed.
    ///
    /// # Errors
    /// Propagates the backing read error.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// Seeking cursor over a [`ReadAt`] source, bounded by the file length and
/// backed by one reusable window.
///
/// The mp4 walk skips a box it does not need by seeking past it, so the
/// `mdat` payload - every byte of the track - is never transferred, and the
/// window is refilled only when the walk lands outside it.
pub(crate) struct ReadAtCursor<'a, R: ReadAt> {
    source: &'a R,
    /// Fixed-size scratch, refilled in place.
    window: [u8; consts::WALK_WINDOW_BYTES],
    pos: u64,
    total: u64,
    window_start: u64,
    /// Bytes of `window` the last refill actually filled.
    window_len: usize,
}

impl<'a, R: ReadAt> ReadAtCursor<'a, R> {
    pub(crate) fn new(source: &'a R, total: u64) -> Self {
        Self {
            source,
            total,
            pos: 0,
            window: [0; consts::WALK_WINDOW_BYTES],
            window_len: 0,
            window_start: 0,
        }
    }

    /// Bytes of the window that start at the cursor, refilling it first when
    /// the cursor sits outside the window it currently holds.
    fn windowed(&mut self) -> io::Result<&[u8]> {
        let offset = self.pos.checked_sub(self.window_start);
        let hit = offset
            .and_then(|offset| usize::try_from(offset).ok())
            .filter(|offset| *offset < self.window_len);
        if hit.is_none() {
            let want = usize::try_from(self.total.saturating_sub(self.pos))
                .unwrap_or(consts::WALK_WINDOW_BYTES)
                .min(consts::WALK_WINDOW_BYTES);
            self.window_len = self.source.read_at(self.pos, &mut self.window[..want])?;
            self.window_start = self.pos;
        }
        let offset = hit.unwrap_or(0);
        self.window
            .get(offset..self.window_len)
            .ok_or_else(|| Error::other("BUG: mp4 walk window offset outside the filled window"))
    }
}

impl<R: ReadAt> Read for ReadAtCursor<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total {
            return Ok(0);
        }
        let available = self.windowed()?;
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.pos = self
            .pos
            .saturating_add(u64::try_from(n).map_err(Error::other)?);
        Ok(n)
    }
}

impl<R: ReadAt> Seek for ReadAtCursor<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.total.checked_add_signed(delta),
        };
        self.pos = target.ok_or_else(|| Error::other("mp4 walk seek out of range"))?;
        Ok(self.pos)
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
}
