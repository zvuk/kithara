use std::io::{self, Error, ErrorKind, Read, Seek, SeekFrom};

use kithara_bufpool::HasPool;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    NotReadyCause, PendingReason, ReadOutcome, SourceError, StreamError, StreamSeekPastEof,
};

use super::{HlsSession, pending};

pub(in crate::stream) struct HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    session: Arc<HlsSession<S>>,
}

impl<S> HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(in crate::stream) const fn new(session: Arc<HlsSession<S>>) -> Self {
        Self { session }
    }

    fn resolve_seek(&self, seek: SeekFrom) -> io::Result<u64> {
        let current = self.session.position();
        let position = match seek {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(delta) => i128::from(current).saturating_add(i128::from(delta)),
            SeekFrom::End(delta) => {
                let len = self.session.len().ok_or_else(|| {
                    Error::new(
                        ErrorKind::Unsupported,
                        "seek from end requires known length",
                    )
                })?;
                i128::from(len).saturating_add(i128::from(delta))
            }
        };
        if position < 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }
        let position = u64::try_from(position).unwrap_or(u64::MAX);
        if let Some(len) = self.session.len()
            && position > len
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                StreamSeekPastEof::new(current, len, position),
            ));
        }
        Ok(position)
    }
}

impl<S> Read for HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.session.check_live()?;
        let position = self.session.projected_position();
        let byte = position.byte;
        let requested_end = byte.saturating_add(buf.len() as u64);
        let unit_end = self
            .session
            .variant
            .init_descriptor_at(byte)
            .map(|range| range.end)
            .or_else(|| {
                self.session
                    .find_at_offset(byte)
                    .map(|(_, start, size)| start.saturating_add(size))
            })
            .unwrap_or(requested_end);
        let read_end = requested_end.min(unit_end);
        let read_len = usize::try_from(read_end.saturating_sub(byte))
            .map_or(buf.len(), |len| len.min(buf.len()));
        let buf = &mut buf[..read_len];
        let range = byte..read_end;
        let wait_budget = if self.session.construction_blocking() {
            None
        } else {
            Some(Duration::ZERO)
        };
        match self.session.wait_range(range, wait_budget) {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => return Ok(0),
            Ok(WaitOutcome::Interrupted) => {
                return Err(pending(PendingReason::SeekPending));
            }
            Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                self.session.arm_peer();
                return Err(pending(PendingReason::NotReady(
                    NotReadyCause::WaitBudgetExhausted,
                )));
            }
            Err(error) => return Err(Error::other(error)),
        }
        self.session.check_live()?;
        match self.session.variant.read_at(byte, buf) {
            Ok(ReadOutcome::Bytes(count)) => {
                self.session.check_live()?;
                self.session.advance_from(position, count.get() as u64);
                Ok(count.get())
            }
            Ok(ReadOutcome::Eof) => Ok(0),
            Ok(ReadOutcome::Pending(reason)) => {
                self.session.arm_peer();
                Err(pending(reason))
            }
            Err(error) => Err(Error::other(error)),
        }
    }
}

impl<S> Seek for HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        self.session.check_live()?;
        let position = self.resolve_seek(seek)?;
        self.session.seek_to_byte(position);
        self.session.arm_peer();
        Ok(position)
    }
}
