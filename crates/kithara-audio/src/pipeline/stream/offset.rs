use std::io::{self, Read, Seek, SeekFrom};

use delegate::delegate;
use kithara_stream::StreamType;

use super::shared::SharedStream;

/// Reader that offsets all positions by a base offset.
///
/// When Symphonia seeks to position X, the real stream position is `base_offset + X`.
/// This is needed when recreating a decoder after ABR variant switch:
/// the new segment starts at `base_offset` in the virtual stream, but Symphonia
/// expects positions starting from 0.
pub(crate) struct OffsetReader<T: StreamType> {
    shared: SharedStream<T>,
    base_offset: u64,
}

impl<T: StreamType> OffsetReader<T> {
    pub(crate) fn new(shared: SharedStream<T>, base_offset: u64) -> Self {
        let _ = shared.probe_seek(SeekFrom::Start(base_offset));
        Self {
            shared,
            base_offset,
        }
    }
}

impl<T: StreamType> Read for OffsetReader<T> {
    delegate! {
        to self.shared {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
        }
    }
}

impl<T: StreamType> Seek for OffsetReader<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // The decoder runs on the produce core, so seek through the real-time
        // `probe_seek` (no `prime_seek_range` spin on the forbid path).
        match pos {
            SeekFrom::Start(p) => {
                let abs = self.base_offset + p;
                let real_pos = self.shared.probe_seek(SeekFrom::Start(abs))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::Current(delta) => {
                let real_pos = self.shared.probe_seek(SeekFrom::Current(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
            SeekFrom::End(delta) => {
                let real_pos = self.shared.probe_seek(SeekFrom::End(delta))?;
                Ok(real_pos.saturating_sub(self.base_offset))
            }
        }
    }
}
