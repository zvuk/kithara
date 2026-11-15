use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use kithara_platform::sync::Arc;
use kithara_stream::{NotReadyCause, PendingReason, ReaderControl, StreamPending};
use symphonia_core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};

/// Adapter that wraps a Read + Seek source as a Symphonia [`MediaSource`].
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
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
    /// MSS refills fit this bound; larger reads are direct packet demand.
    refill_limit: usize,
    range: Option<Range<u64>>,
    /// The source error follows the consumed prefix already returned by this adapter.
    read_error: Option<io::Error>,
    #[field(with, option_set_some, vis = "pub(crate)")]
    control: Option<ReaderControl>,
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
            refill_limit: MediaSourceStreamOptions::default().buffer_len,
            byte_pos: Arc::new(AtomicU64::new(initial_pos)),
            range: None,
            read_error: None,
            control: None,
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
        self.read_error = None;
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

    #[cfg(any(feature = "symphonia", feature = "android"))]
    pub(crate) fn seek_enabled_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.seek_enabled)
    }
}

impl<R: Read + Seek + Send + Sync + 'static> From<ReadSeekAdapter<R>>
    for MediaSourceStream<'static>
{
    fn from(adapter: ReadSeekAdapter<R>) -> Self {
        let options = MediaSourceStreamOptions {
            buffer_len: adapter.refill_limit,
        };
        Self::new(Box::new(adapter), options)
    }
}

impl<R: Read> Read for ReadSeekAdapter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let direct_demand = buf.len() > self.refill_limit;
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
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(error) = self.read_error.take() {
            return Err(error);
        }
        let mut read = 0;
        while read < buf.len() {
            if self
                .control
                .as_ref()
                .is_some_and(ReaderControl::is_cancelled)
            {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    PendingReason::SessionRetired,
                ));
            }
            let readiness = self
                .control
                .as_ref()
                .map(|control| (control, control.revision()));
            match self.inner.read(&mut buf[read..]) {
                Ok(0) => break,
                Ok(len) => {
                    read += len;
                    if let Some(control) = &self.control {
                        control.clear_pending();
                    }
                    if !direct_demand {
                        break;
                    }
                }
                Err(error) => {
                    if resume_pending_read(readiness, &error)? {
                        continue;
                    }
                    if read == 0 {
                        return Err(error);
                    }
                    self.read_error = Some(error);
                    break;
                }
            }
        }
        if read > 0 {
            self.byte_pos.fetch_add(read as u64, Ordering::Release);
        }
        Ok(read)
    }
}

fn resume_pending_read(
    readiness: Option<(&ReaderControl, u64)>,
    error: &io::Error,
) -> io::Result<bool> {
    if let Some((control, revision)) = readiness {
        if let Some(reason) = pending_input(error) {
            control.set_pending(reason);
            if !control.wait(revision) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    PendingReason::SessionRetired,
                ));
            }
            return Ok(true);
        }
        if error.kind() == io::ErrorKind::Interrupted
            && error
                .get_ref()
                .is_none_or(|inner| !inner.is::<PendingReason>())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn pending_input(error: &io::Error) -> Option<PendingReason> {
    if !matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    ) {
        return None;
    }
    let reason = error.get_ref().and_then(|inner| {
        inner
            .downcast_ref::<StreamPending>()
            .map(StreamPending::reason)
            .or_else(|| inner.downcast_ref::<PendingReason>().copied())
    });
    match reason {
        Some(reason @ (PendingReason::NotReady(_) | PendingReason::Retry)) => Some(reason),
        None if error.kind() == io::ErrorKind::WouldBlock => {
            Some(PendingReason::NotReady(NotReadyCause::SourcePending))
        }
        _ => None,
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
            if !matches!(pos, SeekFrom::Current(0)) {
                self.read_error = None;
            }
            self.byte_pos.store(physical, Ordering::Release);
            return Ok(physical - range.start);
        }
        let new_pos = self.inner.seek(pos)?;
        if !matches!(pos, SeekFrom::Current(0)) {
            self.read_error = None;
        }
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
    use std::{io::Cursor, sync::mpsc};

    use kithara_stream::PendingReason;
    use kithara_test_utils::kithara;
    use symphonia_core::io::MediaSource;

    use super::*;

    struct PrefixErrorSource {
        cursor: Cursor<Vec<u8>>,
        error: Option<io::Error>,
        pending_read: Option<mpsc::Sender<Result<usize, io::ErrorKind>>>,
    }

    impl Read for PrefixErrorSource {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.cursor.position() == 3
                && let Some(error) = self.error.take()
            {
                if let Some(pending_read) = &self.pending_read {
                    pending_read
                        .send(Err(error.kind()))
                        .expect("report pending input");
                }
                return Err(error);
            }
            let len = buf.len().min(3);
            self.cursor.read(&mut buf[..len])
        }
    }

    impl Seek for PrefixErrorSource {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    fn prefix_error_adapter(
        controlled: bool,
        terminal: bool,
    ) -> ReadSeekAdapter<PrefixErrorSource> {
        let error = if terminal {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                io::Error::from_raw_os_error(5),
            )
        } else {
            io::Error::new(io::ErrorKind::WouldBlock, PendingReason::Retry)
        };
        let source = PrefixErrorSource {
            cursor: Cursor::new(vec![1, 2, 3, 4, 5, 6]),
            error: Some(error),
            pending_read: None,
        };
        let adapter = ReadSeekAdapter::new(source, None, true);
        if controlled {
            adapter.with_control(ReaderControl::new(None))
        } else {
            adapter
        }
    }

    #[kithara::test(native, flash(false))]
    fn available_prefix_returns_before_waiting_for_future_input() {
        let (observed_tx, observed_rx) = mpsc::channel();
        let mut adapter = prefix_error_adapter(true, false);
        adapter.inner.pending_read = Some(observed_tx.clone());
        let control = adapter.control.as_ref().expect("input control").clone();
        let worker = std::thread::spawn(move || {
            let mut bytes = [0; 6];
            let result = adapter.read(&mut bytes);
            observed_tx
                .send(result.as_ref().copied().map_err(io::Error::kind))
                .expect("report returned prefix");
            (adapter, bytes, result)
        });

        let first = observed_rx.recv();
        control.cancel();
        let (adapter, bytes, result) = worker.join().expect("prefix reader joins");

        assert_eq!(
            first.expect("read progress observed"),
            Ok(3),
            "ready prefix must return before pending input is requested"
        );
        assert_eq!(result.expect("available prefix"), 3);
        assert_eq!(&bytes[..3], &[1, 2, 3]);
        assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
    }

    fn assert_direct_read(range: Option<Range<u64>>) {
        let request_bytes = MediaSourceStreamOptions::default().buffer_len + 17;
        let source_bytes = (0..request_bytes + 128)
            .map(|index| u8::try_from(index % 251).expect("fixture byte"))
            .collect::<Vec<_>>();
        let expected = range.clone().unwrap_or(0..request_bytes as u64);
        let source = PrefixErrorSource {
            cursor: Cursor::new(source_bytes.clone()),
            error: None,
            pending_read: None,
        };
        let mut adapter = ReadSeekAdapter::new(source, None, true);
        if let Some(range) = range {
            adapter = adapter.with_range(range).expect("encoded source range");
        }
        let mut bytes = vec![0xa5; request_bytes];
        let start = usize::try_from(expected.start).expect("range start");
        let end = usize::try_from(expected.end).expect("range end");
        assert_eq!(
            adapter.read(&mut bytes).expect("direct native demand"),
            end - start,
            "direct demand must complete every short tail and clipped range"
        );
        assert_eq!(&bytes[..end - start], &source_bytes[start..end]);
        assert!(bytes[end - start..].iter().all(|byte| *byte == 0xa5));
        assert_eq!(
            adapter.byte_pos_handle().load(Ordering::Acquire),
            expected.end
        );
    }

    #[kithara::test]
    fn direct_reads_complete_short_tails() {
        assert_direct_read(None);
    }

    #[kithara::test]
    fn direct_reads_complete_clipped_ranges() {
        assert_direct_read(Some(5..4_096));
    }

    #[kithara::test]
    fn short_reads_preserve_the_consumed_prefix_and_exact_deferred_error() {
        for (controlled, terminal) in [(false, false), (false, true), (true, true)] {
            for reposition in [false, true] {
                let mut adapter = prefix_error_adapter(controlled, terminal);
                let expected_error = adapter.inner.error.as_ref().expect("source error");
                let expected_kind = expected_error.kind();
                let expected_identity =
                    std::ptr::from_ref(expected_error.get_ref().expect("owned error"));
                let mut bytes = [0; 6];
                assert_eq!(adapter.read(&mut bytes).expect("consumed prefix"), 3);
                assert_eq!(&bytes[..3], &[1, 2, 3]);
                assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
                assert_eq!(adapter.read(&mut []).expect("empty read"), 0);
                assert!(adapter.seek(SeekFrom::Current(-4)).is_err());
                assert_eq!(adapter.stream_position().expect("observed position"), 3);
                if reposition {
                    assert_eq!(adapter.seek(SeekFrom::Start(0)).expect("reposition"), 0);
                    assert_eq!(adapter.read(&mut bytes).expect("repositioned prefix"), 3);
                    assert_eq!(&bytes[..3], &[1, 2, 3]);
                    assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
                }
                let error = adapter
                    .read(&mut bytes)
                    .expect_err("source error after prefix");
                assert_eq!(error.kind(), expected_kind);
                assert!(std::ptr::eq(
                    expected_identity,
                    error.get_ref().expect("same owned error")
                ));
                if !terminal {
                    assert_eq!(
                        error
                            .get_ref()
                            .and_then(|error| error.downcast_ref::<PendingReason>()),
                        Some(&PendingReason::Retry)
                    );
                }
                assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
                assert_eq!(adapter.read(&mut bytes).expect("remaining bytes"), 3);
                assert_eq!(&bytes[..3], &[4, 5, 6]);
                assert_eq!(adapter.read(&mut bytes).expect("physical EOF"), 0);
            }
            let mut adapter = prefix_error_adapter(controlled, terminal);
            let expected_error = adapter.inner.error.as_ref().expect("source error");
            let expected_kind = expected_error.kind();
            let expected_identity =
                std::ptr::from_ref(expected_error.get_ref().expect("owned error"));
            adapter.seek(SeekFrom::Start(3)).expect("error position");
            let mut bytes = [0; 6];
            let error = adapter.read(&mut bytes).expect_err("error before progress");
            assert_eq!(error.kind(), expected_kind);
            assert!(std::ptr::eq(
                expected_identity,
                error.get_ref().expect("same owned error")
            ));
            assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
            assert_eq!(adapter.read(&mut bytes).expect("released source"), 3);
            assert_eq!(&bytes[..3], &[4, 5, 6]);
        }
    }

    #[kithara::test]
    fn direct_reads_preserve_deferred_errors_and_seek_boundaries() {
        for (controlled, terminal) in [(false, false), (false, true), (true, true)] {
            for reposition in [false, true] {
                let mut adapter = prefix_error_adapter(controlled, terminal);
                let expected_error = adapter.inner.error.as_ref().expect("source error");
                let expected_kind = expected_error.kind();
                let expected_identity =
                    std::ptr::from_ref(expected_error.get_ref().expect("owned error"));
                let mut bytes = vec![0; MediaSourceStreamOptions::default().buffer_len + 1];
                assert_eq!(adapter.read(&mut bytes).expect("consumed prefix"), 3);
                assert_eq!(&bytes[..3], &[1, 2, 3]);
                assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
                assert_eq!(adapter.read(&mut []).expect("empty read"), 0);
                assert!(adapter.seek(SeekFrom::Current(-4)).is_err());
                assert_eq!(adapter.stream_position().expect("observed position"), 3);
                if reposition {
                    assert_eq!(adapter.seek(SeekFrom::Start(0)).expect("reposition"), 0);
                    assert_eq!(adapter.read(&mut bytes).expect("repositioned bytes"), 6);
                    assert_eq!(&bytes[..6], &[1, 2, 3, 4, 5, 6]);
                    assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 6);
                } else {
                    let error = adapter.read(&mut bytes).expect_err("deferred source error");
                    assert_eq!(error.kind(), expected_kind);
                    assert!(std::ptr::eq(
                        expected_identity,
                        error.get_ref().expect("same owned error")
                    ));
                    if !terminal {
                        assert_eq!(
                            error
                                .get_ref()
                                .and_then(|error| error.downcast_ref::<PendingReason>()),
                            Some(&PendingReason::Retry)
                        );
                    }
                    assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 3);
                    assert_eq!(adapter.read(&mut bytes).expect("remaining bytes"), 3);
                    assert_eq!(&bytes[..3], &[4, 5, 6]);
                    assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 6);
                }
                assert_eq!(adapter.read(&mut bytes).expect("physical EOF"), 0);
            }
        }
    }

    #[kithara::test]
    fn cancelled_input_is_terminal_while_transient_interruptions_still_resume() {
        let control = ReaderControl::new(None);
        let mut adapter =
            ReadSeekAdapter::new(Cursor::new(vec![1, 2]), None, true).with_control(control.clone());
        control.cancel();
        let error = adapter.read(&mut [0; 1]).expect_err("cancelled reader");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(
            error
                .get_ref()
                .and_then(|source| source.downcast_ref::<PendingReason>()),
            Some(&PendingReason::SessionRetired)
        );
        assert_eq!(adapter.byte_pos_handle().load(Ordering::Acquire), 0);
        let interrupted = io::Error::new(io::ErrorKind::Interrupted, "temporary signal");
        let active = ReaderControl::new(None);
        assert!(
            resume_pending_read(Some((&active, active.revision())), &interrupted)
                .expect("ordinary interruption resumes")
        );
        let pending = io::Error::new(io::ErrorKind::WouldBlock, PendingReason::Retry);
        assert_eq!(
            resume_pending_read(Some((&control, control.revision())), &pending)
                .expect_err("retired pending read")
                .kind(),
            io::ErrorKind::ConnectionAborted
        );
    }

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
