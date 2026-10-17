use std::{
    io::{self, Error, ErrorKind, Read, Seek, SeekFrom},
    ops::{ControlFlow, Range},
};

use kithara_bufpool::HasPool;
use kithara_events::DeferredBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    ByteMap, ConstructionGate, NotReadyCause, OpenedReader, PendingReason, ReadOutcome,
    ReaderControl, SourceError, StreamError, StreamSeekPastEof,
};
use tracing::debug;

use super::{
    HlsSession,
    hls::{SessionPosition, pending},
};
use crate::{HlsEvent, reader::HlsReaderEventSink};

pub(in crate::stream) struct HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    session: Arc<HlsSession<S>>,
    control: ReaderControl,
    construction_gate: Option<ConstructionGate>,
}

impl<S> HlsSessionReader<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(in crate::stream) fn open(
        session: Arc<HlsSession<S>>,
        control: ReaderControl,
        construction_gate: Option<ConstructionGate>,
        emit: Arc<DeferredBus<HlsEvent>>,
    ) -> OpenedReader {
        let reader = Self {
            session: Arc::clone(&session),
            control: control.clone(),
            construction_gate: construction_gate.clone(),
        };
        OpenedReader::new(
            reader,
            session.len(),
            Some(Arc::clone(&session) as Arc<dyn ByteMap>),
            construction_gate,
            control,
            Some(Box::new(HlsReaderEventSink::for_session(emit, session))),
        )
    }

    fn read_end(&self, byte: u64, requested_end: u64) -> u64 {
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
        requested_end.min(unit_end)
    }

    fn wait_for_readable_range(
        &self,
        range: Range<u64>,
        wait_budget: Option<Duration>,
    ) -> io::Result<WaitOutcome> {
        match self
            .session
            .wait_range(range, wait_budget, Some(&self.control))
        {
            Ok(WaitOutcome::Ready) => Ok(WaitOutcome::Ready),
            Ok(WaitOutcome::Eof) => Ok(WaitOutcome::Eof),
            Ok(WaitOutcome::Interrupted) => {
                self.session.check_live()?;
                if self.control.cancel_token().is_cancelled() {
                    return Err(pending(PendingReason::SessionRetired));
                }
                if wait_budget.is_none() {
                    self.session.wake_peer_for_readiness();
                    return Ok(WaitOutcome::Interrupted);
                }
                Err(pending(PendingReason::NotReady(
                    NotReadyCause::WaitInterrupted,
                )))
            }
            Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                if wait_budget.is_none() {
                    self.session.wake_peer_for_readiness();
                    return Ok(WaitOutcome::Interrupted);
                }
                self.session.arm_peer();
                Err(pending(PendingReason::NotReady(
                    NotReadyCause::WaitBudgetExhausted,
                )))
            }
            Err(error) => Err(Error::other(error)),
        }
    }

    fn read_ready(
        &self,
        position: SessionPosition,
        buf: &mut [u8],
        wait_budget: Option<Duration>,
    ) -> io::Result<ControlFlow<usize>> {
        match self.session.variant.read_at(position.byte, buf) {
            Ok(ReadOutcome::Bytes(count)) => {
                self.session.check_live()?;
                self.session.advance_from(position, count.get() as u64);
                if self.session.take_prefetch_resume() {
                    self.session.arm_peer();
                }
                Ok(ControlFlow::Break(count.get()))
            }
            Ok(ReadOutcome::Eof) => Ok(ControlFlow::Break(0)),
            Ok(ReadOutcome::Pending(reason)) => {
                if wait_budget.is_none()
                    && matches!(reason, PendingReason::NotReady(_) | PendingReason::Retry)
                {
                    self.session.wake_peer_for_readiness();
                    return Ok(ControlFlow::Continue(()));
                }
                self.session.arm_peer();
                Err(pending(reason))
            }
            Err(error) => Err(Error::other(error)),
        }
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
            debug!(
                variant = self.session.variant().variant_index_u32(),
                current,
                len,
                position,
                ?seek,
                "refusing a seek past the published end of the variant"
            );
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
        loop {
            self.session.check_live()?;
            let reader_control = &self.control;
            let reader_cancel = reader_control.cancel_token();
            if reader_cancel.is_cancelled() {
                return Err(pending(PendingReason::SessionRetired));
            }
            let position = self.session.projected_position();
            let byte = position.byte;
            let requested_end = byte.saturating_add(buf.len() as u64);
            let read_end = self.read_end(byte, requested_end);
            let read_len = usize::try_from(read_end.saturating_sub(byte))
                .map_or(buf.len(), |len| len.min(buf.len()));
            let buf = &mut buf[..read_len];
            let range = byte..read_end;
            let wait_budget = if self
                .construction_gate
                .as_ref()
                .is_some_and(ConstructionGate::is_armed)
                || reader_control.is_blocking()
            {
                None
            } else {
                Some(Duration::ZERO)
            };
            match self.wait_for_readable_range(range, wait_budget)? {
                WaitOutcome::Ready => {}
                WaitOutcome::Eof => return Ok(0),
                WaitOutcome::Interrupted => continue,
            }
            self.session.check_live()?;
            match self.read_ready(position, buf, wait_budget)? {
                ControlFlow::Break(count) => return Ok(count),
                ControlFlow::Continue(()) => continue,
            }
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
