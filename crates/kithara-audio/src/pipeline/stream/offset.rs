use std::io::{self, Read, Seek, SeekFrom};

use delegate::delegate;
use kithara_stream::SessionReader;

/// Reader that offsets all positions by a base offset.
///
/// When Symphonia seeks to position X, the real stream position is `base_offset + X`.
/// This is needed when recreating a decoder after ABR variant switch:
/// the new segment starts at `base_offset` in the virtual stream, but Symphonia
/// expects positions starting from 0.
pub(crate) struct OffsetReader<R: SessionReader> {
    shared: R,
    base_offset: u64,
}

impl<R: SessionReader> OffsetReader<R> {
    pub(crate) fn new(mut shared: R, base_offset: u64) -> io::Result<Self> {
        shared.seek(SeekFrom::Start(base_offset))?;
        Ok(Self {
            shared,
            base_offset,
        })
    }
}

impl<R: SessionReader> Read for OffsetReader<R> {
    delegate! {
        to self.shared {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
        }
    }
}

impl<R: SessionReader> Seek for OffsetReader<R> {
    /// Preserve the shared reader's blocking seek mode during an owning-thread search.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match pos {
            SeekFrom::Start(offset) => {
                let absolute = self.base_offset.checked_add(offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "decoder seek offset overflow")
                })?;
                let real_pos = self.shared.seek(SeekFrom::Start(absolute))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::Current(delta) => {
                let real_pos = self.shared.seek(SeekFrom::Current(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::End(delta) => {
                let real_pos = self.shared.seek(SeekFrom::End(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
        }
    }
}
