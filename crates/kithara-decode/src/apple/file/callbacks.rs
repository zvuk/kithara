use std::{
    cell::{Cell, RefCell},
    io::{Error, ErrorKind, Read, Seek, SeekFrom},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use kithara_apple::audio_toolbox::{
    AudioFileCallbacks, AudioFilePacketRead, OSStatus, PARAM_ERR, SInt64, UInt32,
};
use kithara_bufpool::ByteBuffer;
use kithara_platform::sync::Arc;
use kithara_stream::PendingReason;

use crate::{
    apple::consts::os_status_to_string,
    error::{DecodeError, DecodeResult, ErrorClass},
    traits::BoxedSource,
};

pub(super) enum SizeMode {
    Unknown,
    /// Authoritative total extent, including bytes still being downloaded.
    Snapshot(i64),
}

pub(super) struct CallbackCtx {
    source: RefCell<BoxedSource>,
    packet_window: RefCell<Option<PacketWindow>>,
    /// Preserve source error chains that `AudioFileServices` replaces with an `OSStatus`.
    pub(super) last_error: Cell<Option<Error>>,
    pub(super) terminal_short_read: Cell<Option<TerminalShortRead>>,
    pub(super) byte_len: OnceLock<Arc<AtomicU64>>,
    size: SizeMode,
}

struct PacketWindow {
    bytes: ByteBuffer,
    active: Option<StagedRange>,
}

struct StagedRange {
    start: u64,
    len: usize,
    end: u64,
    at_eof: bool,
    position: u64,
}

impl CallbackCtx {
    pub(super) fn new(source: BoxedSource, size: SizeMode) -> Self {
        Self {
            source: RefCell::new(source),
            packet_window: RefCell::new(None),
            last_error: Cell::new(None),
            terminal_short_read: Cell::new(None),
            byte_len: OnceLock::new(),
            size,
        }
    }

    pub(super) fn set_packet_window(&self, bytes: ByteBuffer) {
        *self.packet_window.borrow_mut() = Some(PacketWindow {
            bytes,
            active: None,
        });
    }

    pub(super) fn stage_packet(&self, start: u64) -> DecodeResult<()> {
        let mut owned_window = self.packet_window.borrow_mut();
        let Some(window) = owned_window.as_mut() else {
            return Err(DecodeError::InvalidData {
                detail: "native FLAC packet window is missing",
            });
        };
        window.active = None;
        let SizeMode::Snapshot(end) = self.size else {
            return Err(DecodeError::InvalidData {
                detail: "native FLAC staging requires a declared extent",
            });
        };
        let limit = usize::try_from(
            u64::try_from(end)
                .map_err(DecodeError::backend)?
                .saturating_sub(start),
        )
        .map_or(window.bytes.len(), |len| len.min(window.bytes.len()));
        let mut source = self.source.borrow_mut();
        let previous = source.stream_position().map_err(DecodeError::backend)?;
        let read = source
            .seek(SeekFrom::Start(start))
            .map_err(DecodeError::backend)
            .and_then(|_| Self::fill_window(&mut source, &mut window.bytes[..limit]));
        if let Err(error) = &read
            && (error.classify() == ErrorClass::VariantChange
                || matches!(
                    error.pending_reason(),
                    Some(PendingReason::VariantChange | PendingReason::SessionRetired)
                ))
        {
            return read.map(|_| ());
        }
        source
            .seek(SeekFrom::Start(previous))
            .map_err(DecodeError::backend)?;
        let len = read?;
        if len < limit {
            return Err(DecodeError::backend(Error::new(
                ErrorKind::UnexpectedEof,
                "native FLAC source ended before its declared extent",
            )));
        }
        let end = start
            .checked_add(u64::try_from(len).map_err(DecodeError::backend)?)
            .ok_or(DecodeError::InvalidData {
                detail: "native FLAC staged range overflow",
            })?;
        window.active = Some(StagedRange {
            start,
            len,
            end,
            at_eof: len < window.bytes.len(),
            position: previous,
        });
        Ok(())
    }

    fn fill_window(source: &mut BoxedSource, bytes: &mut [u8]) -> DecodeResult<usize> {
        let mut filled = 0;
        while filled < bytes.len() {
            let count = source
                .read(&mut bytes[filled..])
                .map_err(DecodeError::backend)?;
            if count == 0 {
                break;
            }
            filled += count;
        }
        Ok(filled)
    }

    pub(super) fn finish_read(
        &self,
        read: Result<AudioFilePacketRead, OSStatus>,
        operation: &str,
    ) -> DecodeResult<AudioFilePacketRead> {
        let staged = self
            .packet_window
            .borrow()
            .as_ref()
            .is_some_and(|window| window.active.is_some());
        if let Some(error) = self.last_error.take() {
            let error = DecodeError::backend(error);
            if staged || error.pending_reason().is_some() {
                return Err(error);
            }
        }
        read.map_err(|status| {
            DecodeError::backend(Error::other(format!(
                "{operation} failed: {}",
                os_status_to_string(status)
            )))
        })
    }

    pub(super) fn finish_packet(&self) -> DecodeResult<()> {
        let position = self
            .packet_window
            .borrow_mut()
            .as_mut()
            .and_then(|window| window.active.take())
            .map(|range| range.position);
        if let Some(position) = position {
            self.source
                .borrow_mut()
                .seek(SeekFrom::Start(position))
                .map_err(DecodeError::backend)?;
        }
        Ok(())
    }
}

impl PacketWindow {
    fn read_at(&mut self, position: u64, output: &mut [u8]) -> Option<std::io::Result<UInt32>> {
        let range = self.active.as_mut()?;
        let Some(relative) = position.checked_sub(range.start) else {
            return Some(Err(Error::other(
                "native FLAC read precedes its staged window",
            )));
        };
        let offset = match usize::try_from(relative) {
            Ok(offset) => offset,
            Err(error) => return Some(Err(Error::other(error))),
        };
        if offset >= range.len && range.at_eof {
            range.position = range.end;
            return Some(Ok(0));
        }
        if offset > range.len {
            return Some(Err(Error::other(
                "native FLAC read exceeds its staged window",
            )));
        }
        let available = range.len.saturating_sub(offset);
        if available < output.len() && !range.at_eof {
            return Some(Err(Error::other(
                "native FLAC read exceeds its staged window",
            )));
        }
        let count = available.min(output.len());
        output[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        let Ok(count_u32) = UInt32::try_from(count) else {
            return Some(Err(Error::other("native FLAC staged read exceeds u32")));
        };
        let Some(end) = position.checked_add(u64::from(count_u32)) else {
            return Some(Err(Error::other("native FLAC staged read offset overflow")));
        };
        range.position = end;
        Some(Ok(count_u32))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct TerminalShortRead {
    end: u64,
    position: u64,
    request: usize,
}

impl AudioFileCallbacks for CallbackCtx {
    fn read_at(&mut self, position: SInt64, buffer: &mut [u8]) -> Result<UInt32, OSStatus> {
        const UNKNOWN_SIZE_TAIL_PROBE_MIN: SInt64 = SInt64::MAX / 2;
        let Ok(pos) = u64::try_from(position) else {
            self.last_error.set(Some(Error::other(format!(
                "AppleAudioFile read_callback: negative position {position}"
            ))));
            return Err(PARAM_ERR);
        };
        if matches!(self.size, SizeMode::Unknown) && position >= UNKNOWN_SIZE_TAIL_PROBE_MIN {
            return Ok(0);
        }
        let buffer = match self.size {
            SizeMode::Snapshot(end) => {
                let available = usize::try_from(end.saturating_sub(position).max(0))
                    .map_or(buffer.len(), |available| available.min(buffer.len()));
                if available == 0 {
                    return Ok(0);
                }
                &mut buffer[..available]
            }
            SizeMode::Unknown => buffer,
        };
        if let Some(window) = self.packet_window.get_mut()
            && let Some(read) = window.read_at(pos, buffer)
        {
            return read.map_err(|error| {
                self.last_error.set(Some(error));
                PARAM_ERR
            });
        }
        let request = buffer.len();
        if let Some(previous) = self.terminal_short_read.get()
            && previous.position == pos
            && previous.request == request
            && self
                .byte_len
                .get()
                .is_some_and(|byte_len| byte_len.load(Ordering::Acquire) == previous.end)
        {
            return Ok(0);
        }
        if let Err(err) = self.source.get_mut().seek(SeekFrom::Start(pos)) {
            self.terminal_short_read.set(None);
            self.last_error.set(Some(err));
            return Err(PARAM_ERR);
        }
        let n = match self.source.get_mut().read(buffer) {
            Ok(n) => n,
            Err(err) => {
                self.terminal_short_read.set(None);
                self.last_error.set(Some(err));
                return Err(PARAM_ERR);
            }
        };
        let end = u64::try_from(n).ok().and_then(|read| pos.checked_add(read));
        let terminal_short_read = match end {
            Some(end)
                if n > 0
                    && n < request
                    && self
                        .byte_len
                        .get()
                        .is_some_and(|byte_len| byte_len.load(Ordering::Acquire) == end) =>
            {
                Some(TerminalShortRead {
                    request,
                    end,
                    position: pos,
                })
            }
            _ => None,
        };
        self.terminal_short_read.set(terminal_short_read);
        let Ok(n_u32) = UInt32::try_from(n) else {
            self.terminal_short_read.set(None);
            self.last_error.set(Some(Error::other(
                "AppleAudioFile read_callback: bytes read > u32::MAX",
            )));
            return Err(PARAM_ERR);
        };
        Ok(n_u32)
    }

    fn size(&self) -> SInt64 {
        match &self.size {
            SizeMode::Snapshot(size) => *size,
            SizeMode::Unknown => {
                self.last_error.set(Some(Error::other(
                    "AppleAudioFile get_size_callback called without a known size",
                )));
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Error, ErrorKind, Read, Seek, SeekFrom};

    use kithara_apple::audio_toolbox::{AudioFileCallbacks, AudioFilePacketRead};
    use kithara_stream::{NotReadyCause, PendingReason, VariantChangeError};
    use kithara_test_utils::kithara;

    use super::{CallbackCtx, SizeMode};
    use crate::{error::ErrorClass, traits::BoxedSource};

    struct FragmentedSource {
        inner: Cursor<Vec<u8>>,
        max_read: usize,
        error: Option<Error>,
        interrupt_during_seek: bool,
    }

    impl Read for FragmentedSource {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupt_during_seek
                && let Some(error) = self.error.take()
            {
                self.inner.set_position(30);
                return Err(error);
            }
            let count = buffer.len().min(self.max_read);
            self.inner.read(&mut buffer[..count])
        }
    }

    impl Seek for FragmentedSource {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            if self.interrupt_during_seek
                && matches!(position, SeekFrom::Start(8))
                && let Some(error) = self.error.take()
            {
                self.inner.set_position(30);
                return Err(error);
            }
            self.inner.seek(position)
        }
    }

    fn context(source: BoxedSource, size: SizeMode) -> CallbackCtx {
        let ctx = CallbackCtx::new(source, size);
        let pools = crate::test_pools::pools();
        ctx.set_packet_window(pools.get_with_len::<u8>(8).expect("packet window"));
        ctx
    }

    #[kithara::test]
    fn staged_partial_reads_publish_the_served_position() {
        let mut source = FragmentedSource {
            inner: Cursor::new((0..40).collect()),
            max_read: 3,
            error: None,
            interrupt_during_seek: false,
        };
        source.inner.set_position(3);
        let mut ctx = context(Box::new(source), SizeMode::Snapshot(40));
        ctx.stage_packet(8).expect("assemble short positive reads");
        assert_eq!(ctx.size(), 40);
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            3
        );
        let mut output = [0; 3];
        assert_eq!(ctx.read_at(10, &mut output), Ok(3));
        assert_eq!(output, [10, 11, 12]);
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            3
        );
        ctx.finish_packet().expect("publish callback cursor");
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            13
        );
        assert!(
            ctx.packet_window
                .borrow()
                .as_ref()
                .expect("window")
                .active
                .is_none()
        );
    }

    #[kithara::test]
    fn staged_actual_eof_shortens_only_the_final_read() {
        let source = FragmentedSource {
            inner: Cursor::new((0..11).collect()),
            max_read: 2,
            error: None,
            interrupt_during_seek: false,
        };
        let mut ctx = context(Box::new(source), SizeMode::Snapshot(11));
        ctx.stage_packet(8).expect("stage the final bytes");
        let mut output = [0; 8];
        assert_eq!(ctx.read_at(9, &mut output), Ok(2));
        assert_eq!(&output[..2], &[9, 10]);
        assert_eq!(ctx.read_at(11, &mut output), Ok(0));
        ctx.finish_packet().expect("finish at actual EOF");
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            11
        );
    }

    #[kithara::test]
    fn unknown_length_short_reads_do_not_publish_a_partial_extent() {
        let source = FragmentedSource {
            inner: Cursor::new((0..40).collect()),
            max_read: 3,
            error: None,
            interrupt_during_seek: false,
        };
        let mut ctx = CallbackCtx::new(Box::new(source), SizeMode::Unknown);
        let mut output = [0; 8];
        assert_eq!(ctx.read_at(8, &mut output), Ok(3));
        assert_eq!(&output[..3], &[8, 9, 10]);
        assert_eq!(ctx.read_at(11, &mut output), Ok(3));
        assert_eq!(&output[..3], &[11, 12, 13]);
        assert_eq!(ctx.read_at(40, &mut output), Ok(0));
        assert_eq!(ctx.read_at(11, &mut output), Ok(3));
        assert_eq!(&output[..3], &[11, 12, 13]);
    }

    #[kithara::test]
    fn declared_extent_rejects_a_premature_source_eof() {
        let mut source = Cursor::new((0..11).collect::<Vec<u8>>());
        source.set_position(3);
        let ctx = context(Box::new(source), SizeMode::Snapshot(40));
        let error = ctx.stage_packet(8).expect_err("declared bytes are missing");
        assert!(
            error
                .to_string()
                .contains("ended before its declared extent")
        );
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            3
        );
        assert!(
            ctx.packet_window
                .borrow()
                .as_ref()
                .expect("window")
                .active
                .is_none()
        );
    }

    #[kithara::test]
    #[case::data_pending(
        Error::new(
            ErrorKind::Interrupted,
            PendingReason::NotReady(NotReadyCause::SourcePending)
        ),
        Some(PendingReason::NotReady(NotReadyCause::SourcePending)),
        3,
        false
    )]
    #[case::variant_pending(
        Error::new(ErrorKind::Interrupted, PendingReason::VariantChange),
        Some(PendingReason::VariantChange),
        30,
        false
    )]
    #[case::session_retired(
        Error::new(ErrorKind::Interrupted, PendingReason::SessionRetired),
        Some(PendingReason::SessionRetired),
        30,
        false
    )]
    #[case::variant_error(Error::other(VariantChangeError), None, 30, false)]
    #[case::data_pending_during_seek(
        Error::new(
            ErrorKind::Interrupted,
            PendingReason::NotReady(NotReadyCause::SourcePending)
        ),
        Some(PendingReason::NotReady(NotReadyCause::SourcePending)),
        3,
        true
    )]
    #[case::variant_pending_during_seek(
        Error::new(ErrorKind::Interrupted, PendingReason::VariantChange),
        Some(PendingReason::VariantChange),
        30,
        true
    )]
    #[case::session_retired_during_seek(
        Error::new(ErrorKind::Interrupted, PendingReason::SessionRetired),
        Some(PendingReason::SessionRetired),
        30,
        true
    )]
    #[case::variant_error_during_seek(Error::other(VariantChangeError), None, 30, true)]
    fn preflight_preserves_control_invalidation(
        #[case] source_error: Error,
        #[case] pending: Option<PendingReason>,
        #[case] expected_position: u64,
        #[case] interrupt_during_seek: bool,
    ) {
        let mut source = FragmentedSource {
            inner: Cursor::new((0..40).collect()),
            max_read: 3,
            error: Some(source_error),
            interrupt_during_seek,
        };
        source.inner.set_position(3);
        let ctx = context(Box::new(source), SizeMode::Snapshot(40));
        let error = ctx.stage_packet(8).expect_err("preflight interruption");
        assert_eq!(error.pending_reason(), pending);
        if pending.is_none() {
            assert_eq!(error.classify(), ErrorClass::VariantChange);
        }
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            expected_position
        );
        assert!(
            ctx.packet_window
                .borrow()
                .as_ref()
                .expect("window")
                .active
                .is_none()
        );
        ctx.stage_packet(8)
            .expect("next operation stages fresh bytes");
        ctx.finish_packet().expect("finish the next operation");
    }

    #[kithara::test]
    #[case::masked_eof(AudioFilePacketRead { bytes: 0, packets: 0 }, SizeMode::Snapshot(40), 15)]
    #[case::masked_truncation(AudioFilePacketRead { bytes: 2, packets: 1 }, SizeMode::Snapshot(40), 15)]
    fn staged_span_violation_survives_native_success(
        #[case] native_read: AudioFilePacketRead,
        #[case] size: SizeMode,
        #[case] position: i64,
    ) {
        let mut source = Cursor::new((0..40).collect::<Vec<u8>>());
        source.set_position(3);
        let mut ctx = context(Box::new(source), size);
        ctx.stage_packet(8).expect("stage bounded data");
        let mut output = [0; 2];
        assert!(ctx.read_at(position, &mut output).is_err());
        let error = ctx
            .finish_read(Ok(native_read), "AudioFileReadPacketData")
            .expect_err("native success must preserve its callback failure");
        assert!(error.to_string().contains("exceeds its staged window"));
        assert_eq!(
            ctx.source
                .borrow_mut()
                .stream_position()
                .expect("source cursor"),
            3
        );
        ctx.finish_packet().expect("clear failed native operation");
        assert!(
            ctx.packet_window
                .borrow()
                .as_ref()
                .expect("window")
                .active
                .is_none()
        );
    }
}
