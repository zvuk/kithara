pub(super) use std::{
    collections::VecDeque,
    io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll, Waker},
};

pub(super) use kithara_platform::sync::Arc;
pub(super) use kithara_storage::WaitOutcome;
pub(super) use kithara_test_utils::kithara;

pub(super) use super::{
    super::super::{StreamReadOutcome, read::WaitMode},
    *,
};
pub(super) use crate::{
    ActivityWriter, NotReadyCause, PendingReason, PlayheadRead, PlayheadState, ReadOutcome, Source,
    SourcePhase, SourceProbe, StreamError,
};

/// Test helper — script entry that maps to either `Bytes(N)` (with
/// the source slicing actual `data`) or a terminal `Eof`. Pending
/// causes are exercised through the seek state (`begin`) and
/// the wait-outcome script, not the read script.
#[derive(Clone, Copy)]
pub(super) enum ScriptRead {
    Retry,
    Data(usize),
    Eof,
}

pub(super) fn bytes(count: usize) -> ReadOutcome {
    let nz =
        NonZeroUsize::new(count).expect("BUG: ScriptSource::bytes invariant — count must be > 0");
    ReadOutcome::Bytes(nz)
}

/// Constant-phase probe for the scripted test sources below; shares the
/// source's cursor cell and mirrors its `len`. The byte map is not
/// scripted — nothing in these tests reads it through the probe.
pub(super) struct FixedProbe {
    position: Arc<AtomicU64>,
    len: Option<u64>,
    phase: SourcePhase,
}

impl SourceProbe for FixedProbe {
    fn byte_map(&self) -> Option<Arc<dyn crate::ByteMap>> {
        None
    }
    fn len(&self) -> Option<u64> {
        self.len
    }
    fn phase(&self) -> SourcePhase {
        self.phase
    }
    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        self.phase
    }
    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }
    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(super) struct ScriptSource {
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    activity: Activity,
    activity_writer: Option<ActivityWriter>,
    anchor: Option<SourceSeekAnchor>,
    #[field(with, option_set_some, vis = "pub(super)")]
    peer_wake: Option<Arc<DeferredWake>>,
    ready_end: Option<u64>,
    data: Vec<u8>,
    segments: Vec<Range<u64>>,
    reads: VecDeque<ScriptRead>,
    waits: VecDeque<StreamResult<WaitOutcome>>,
    pub(super) waited: Vec<Range<u64>>,
}

impl ScriptSource {
    pub(super) fn new(
        activity: ActivityWriter,
        waits: impl IntoIterator<Item = WaitOutcome>,
        reads: impl IntoIterator<Item = ScriptRead>,
        data: Vec<u8>,
    ) -> Self {
        Self {
            activity: activity.reader(),
            activity_writer: Some(activity),
            data,
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            anchor: None,
            reads: reads.into_iter().collect(),
            ready_end: None,
            segments: Vec::new(),
            waits: waits.into_iter().map(Ok).collect(),
            waited: Vec::new(),
            peer_wake: None,
        }
    }

    pub(super) fn with_segments(
        mut self,
        segments: impl IntoIterator<Item = Range<u64>>,
        ready_end: u64,
    ) -> Self {
        self.segments = segments.into_iter().collect();
        self.ready_end = Some(ready_end);
        self
    }
}

impl Source for ScriptSource {
    fn activity(&self) -> Activity {
        self.activity.clone()
    }

    fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.activity_writer.take()
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn byte_map(&self) -> Option<Arc<dyn crate::ByteMap>> {
        Some(Arc::new(ScriptByteMap {
            anchor: self.anchor,
            len: self.data.len() as u64,
            segments: self.segments.clone(),
        }))
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        self.peer_wake.clone()
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Waiting
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(FixedProbe {
            phase: SourcePhase::Waiting,
            len: Some(self.data.len() as u64),
            position: Arc::clone(&self.position),
        })
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let step = self.reads.pop_front().unwrap_or(ScriptRead::Eof);
        match step {
            ScriptRead::Retry => Ok(ReadOutcome::Pending(PendingReason::Retry)),
            ScriptRead::Eof => Ok(ReadOutcome::Eof),
            ScriptRead::Data(n) => {
                let Ok(start) = usize::try_from(offset) else {
                    return Ok(ReadOutcome::Eof);
                };
                let end = (start + n).min(self.data.len());
                let bytes_count = end.saturating_sub(start).min(buf.len());
                if bytes_count == 0 {
                    return Ok(ReadOutcome::Eof);
                }
                buf[..bytes_count].copy_from_slice(&self.data[start..start + bytes_count]);
                Ok(bytes(bytes_count))
            }
        }
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn wait_range(
        &mut self,
        range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        self.waited.push(range.clone());
        if self
            .ready_end
            .is_some_and(|ready_end| range.end > ready_end)
        {
            return Err(SourceError::WaitBudgetExceeded.into());
        }
        self.waits.pop_front().unwrap_or(Ok(WaitOutcome::Ready))
    }
}

pub(super) struct ScriptByteMap {
    anchor: Option<SourceSeekAnchor>,
    segments: Vec<Range<u64>>,
    len: u64,
}

impl ScriptByteMap {
    fn descriptor(&self, index: usize) -> Option<crate::SegmentDescriptor> {
        let segment_index = u32::try_from(index).ok()?;
        Some(
            crate::SegmentDescriptor::builder()
                .byte_range(self.segments.get(index)?.clone())
                .decode_time(Duration::ZERO)
                .duration(Duration::ZERO)
                .segment_index(segment_index)
                .variant_index(0)
                .build(),
        )
    }
}

impl crate::ByteMap for ScriptByteMap {
    fn anchor_at_time(&self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
        Ok(self.anchor)
    }

    fn init_segment_range(&self) -> Range<u64> {
        0..0
    }

    fn len(&self) -> Option<u64> {
        Some(self.len)
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<crate::SegmentDescriptor> {
        self.segments
            .iter()
            .position(|range| range.start >= byte_offset)
            .and_then(|index| self.descriptor(index))
    }

    fn segment_at_byte(&self, byte_offset: u64) -> Option<crate::SegmentDescriptor> {
        self.segments
            .iter()
            .position(|range| range.contains(&byte_offset))
            .and_then(|index| self.descriptor(index))
    }

    fn segment_at_time(&self, _t: Duration) -> Option<crate::SegmentDescriptor> {
        None
    }

    fn segment_count(&self) -> Option<u32> {
        None
    }
}

pub(super) struct DummyType;

impl StreamType for DummyType {
    type Config = ScriptSource;
    type Events = ();
    type Source = ScriptSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        Ok(config)
    }
}

#[kithara::test]
fn completed_source_construction_does_not_wait_for_a_scheduler_turn() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Ready],
        [ScriptRead::Data(4)],
        vec![1, 2, 3, 4],
    );
    let position = Arc::clone(&source.position);
    let mut open = std::pin::pin!(Stream::<DummyType>::new(source));
    let mut context = Context::from_waker(Waker::noop());

    let Poll::Ready(result) = open.as_mut().poll(&mut context) else {
        panic!("completed source creation must return without another scheduling event");
    };
    let stream = result.expect("the configured source is already constructed");
    assert!(Arc::ptr_eq(&stream.source.position, &position));
    assert_eq!(stream.len(), Some(4));
    assert_eq!(stream.phase_at(0..4), SourcePhase::Waiting);
}

pub(super) struct SeekDuringWaitType;

impl StreamType for SeekDuringWaitType {
    type Config = ();
    type Events = ();
    type Source = SeekDuringWaitSource;

    async fn create(_config: Self::Config) -> Result<Self::Source, SourceError> {
        Err(SourceError::other(IoError::other("not used in unit tests")))
    }
}

pub(super) struct SeekDuringWaitSource {
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    activity: Activity,
    activity_writer: Option<ActivityWriter>,
    read_calls: usize,
}

impl Source for SeekDuringWaitSource {
    fn activity(&self) -> Activity {
        self.activity.clone()
    }

    fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.activity_writer.take()
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn len(&self) -> Option<u64> {
        Some(4)
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(FixedProbe {
            phase: SourcePhase::Ready,
            len: Some(4),
            position: Arc::clone(&self.position),
        })
    }

    fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        self.read_calls += 1;
        Ok(bytes(4))
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        Err(SourceError::Io(IoError::new(
            ErrorKind::Interrupted,
            PendingReason::SessionRetired,
        ))
        .into())
    }
}

#[kithara::test]
fn probe_read_yields_retry_before_consuming_more_source_steps() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Ready, WaitOutcome::Ready],
        [ScriptRead::Retry, ScriptRead::Data(4)],
        vec![1, 2, 3, 4],
    );
    let mut stream = Stream::<DummyType> { source };
    let mut out = [0; 4];
    assert!(matches!(
        stream.try_read(&mut out),
        Ok(StreamReadOutcome::Pending(PendingReason::Retry))
    ));
    assert_eq!(stream.source.position(), 0);
    assert_eq!(stream.source.reads.len(), 1);
    assert!(matches!(
        stream.try_read(&mut out),
        Ok(StreamReadOutcome::Bytes { .. })
    ));
    assert_eq!(out, [1, 2, 3, 4]);
}

#[kithara::test]
fn probe_wait_probes_the_source_without_reading() {
    // The parked reader's demand channel: a single zero-budget
    // `Source::wait_range` probe — readiness without blocking, and the
    // cursor stays put so it can never masquerade as a read.
    let source = ScriptSource::new(ActivityWriter::new(), [], [], Vec::new())
        .with_segments([Range { start: 0, end: 8 }], 4);
    let mut stream = Stream::<DummyType> { source };

    let ready = probe_wait(&mut stream, 0..4);
    assert!(
        matches!(ready, Ok(WaitOutcome::Ready)),
        "a resident range answers Ready: {ready:?}"
    );

    let parked = probe_wait(&mut stream, 0..8);
    assert!(
        matches!(
            parked,
            Err(StreamError::Source(SourceError::WaitBudgetExceeded))
        ),
        "a not-ready range surfaces the typed budget error: {parked:?}"
    );
    assert_eq!(
        stream.source.position.load(Ordering::Acquire),
        0,
        "a wait probe never advances the cursor"
    );
}

#[kithara::test]
fn probe_read_arms_peer_wake_on_core_without_notifying() {
    // Worker read path: a not-ready probe ARMS the deferred wake (lock-free)
    // instead of waking the downloader cross-thread — that `notify_one` is a
    // `kevent` the RT produce core must not make. The scheduler shell flushes
    let wake = Arc::new(DeferredWake::default());
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Interrupted],
        [],
        vec![0u8; 8],
    )
    .with_peer_wake(Arc::clone(&wake));
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 4];

    let outcome = stream.probe_read(&mut buf);
    assert!(
        outcome.is_err(),
        "not-ready worker probe surfaces as an Interrupted io::Error"
    );
    assert!(
        wake.flush(),
        "the worker probe armed the deferred wake; the shell flush delivers it"
    );
    assert!(!wake.flush(), "the arm coalesced into a single delivery");
}

#[kithara::test]
fn read_notifies_peer_wake_immediately_off_core() {
    // Consumer read path: a not-ready probe wakes the peer IMMEDIATELY (the
    // consumer is off the RT core), so a blocking read is not stalled
    // waiting for the worker's next pass. An immediate notify leaves nothing
    // armed — the distinguishing signal from the worker's deferred arm.
    let wake = Arc::new(DeferredWake::default());
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Interrupted, WaitOutcome::Ready],
        [ScriptRead::Data(4)],
        b"ABCD".to_vec(),
    )
    .with_peer_wake(Arc::clone(&wake));
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 4];

    let n = stream
        .read(&mut buf)
        .expect("read completes once the source reports the range ready");
    assert_eq!(n, 4);
    assert_eq!(&buf, b"ABCD");
    assert!(
        !wake.flush(),
        "the consumer path notifies immediately — nothing is left armed"
    );
}

#[kithara::test]
fn try_read_yields_not_ready_on_interrupted_then_recovers_next_probe() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [WaitOutcome::Interrupted, WaitOutcome::Ready],
        [ScriptRead::Data(4)],
        b"ABCD".to_vec(),
    );
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 4];

    let first = stream
        .try_read(&mut buf)
        .expect("BUG: not-ready is a status return; not a hard error in this test");
    assert!(matches!(
        first,
        StreamReadOutcome::Pending(PendingReason::NotReady(NotReadyCause::WaitInterrupted))
    ));

    let n = stream
        .read(&mut buf)
        .expect("BUG: read must succeed once the source reports the range ready");
    assert_eq!(n, 4);
    assert_eq!(&buf, b"ABCD");
}

#[kithara::test]
fn try_read_stops_at_ready_segment_boundary() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [],
        [ScriptRead::Data(8)],
        b"ABCDEFGH".to_vec(),
    )
    .with_segments([0..4, 4..8], 4)
    .with_peer_wake(Arc::new(DeferredWake::default()));
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 8];

    let outcome = stream
        .try_read(&mut buf)
        .expect("the ready current segment must produce a partial read");
    let StreamReadOutcome::Bytes {
        count,
        byte_position,
    } = outcome
    else {
        panic!("ready current segment must not wait for the next segment: {outcome:?}");
    };

    assert_eq!(count.get(), 4);
    assert_eq!(byte_position, 4);
    assert_eq!(&buf[..4], b"ABCD");
    assert_eq!(stream.position(), 4);

    let next = stream
        .try_read(&mut buf)
        .expect("the unavailable next segment is a pending status");
    assert!(matches!(
        next,
        StreamReadOutcome::Pending(PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted))
    ));
    assert_eq!(stream.position(), 4);
}

#[kithara::test]
fn blocking_read_stops_at_ready_segment_boundary() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [],
        [ScriptRead::Data(8)],
        b"ABCDEFGH".to_vec(),
    )
    .with_segments([0..4, 4..8], 4)
    .with_peer_wake(Arc::new(DeferredWake::default()));
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 8];

    let outcome = stream
        .try_read_with(&mut buf, WaitMode::Block)
        .expect("the ready current segment must produce a partial read");
    let StreamReadOutcome::Bytes {
        count,
        byte_position,
    } = outcome
    else {
        panic!("a blocking read must not park on a segment past the cursor: {outcome:?}");
    };

    assert_eq!(count.get(), 4);
    assert_eq!(byte_position, 4);
    assert_eq!(&buf[..4], b"ABCD");
}

#[kithara::test]
fn blocking_read_reports_not_ready_when_its_own_segment_is_unready() {
    let source = ScriptSource::new(
        ActivityWriter::new(),
        [],
        [ScriptRead::Data(8)],
        b"ABCDEFGH".to_vec(),
    )
    .with_segments([0..4, 4..8], 4)
    .with_peer_wake(Arc::new(DeferredWake::default()));
    let mut stream = Stream::<DummyType> { source };
    stream.source.set_position(4);
    let mut buf = [0u8; 8];

    let outcome = stream
        .try_read_with(&mut buf, WaitMode::Block)
        .expect("an unready segment under the cursor is a pending status");
    assert!(matches!(
        outcome,
        StreamReadOutcome::Pending(PendingReason::NotReady(NotReadyCause::WaitBudgetExhausted))
    ));
    assert_eq!(stream.position(), 4);
}

#[kithara::test]
fn try_read_returns_seek_pending_when_flushing() {
    let mut source = ScriptSource::new(ActivityWriter::new(), [], [], vec![]);
    source.waits.push_back(Err(SourceError::Io(IoError::new(
        ErrorKind::Interrupted,
        PendingReason::SessionRetired,
    ))
    .into()));
    let mut stream = Stream::<DummyType> { source };
    let mut buf = [0u8; 4];

    let outcome = stream
        .try_read(&mut buf)
        .expect("BUG: seek-pending is a status return; not a hard error in this test");
    assert!(matches!(
        outcome,
        StreamReadOutcome::Pending(PendingReason::SessionRetired)
    ));
}

#[kithara::test]
fn try_read_returns_seek_pending_when_epoch_changes_after_wait() {
    let writer = ActivityWriter::new();
    let source = SeekDuringWaitSource {
        activity: writer.reader(),
        activity_writer: Some(writer),
        playhead: Arc::new(PlayheadState::new()),
        position: Arc::new(AtomicU64::new(0)),
        read_calls: 0,
    };
    let mut stream = Stream::<SeekDuringWaitType> { source };
    let mut buf = [0u8; 4];

    let outcome = stream
        .try_read(&mut buf)
        .expect("BUG: seek-pending is a status return; not a hard error in this test");

    assert!(matches!(
        outcome,
        StreamReadOutcome::Pending(PendingReason::SessionRetired)
    ));
    assert_eq!(stream.source.read_calls, 0);
    assert_eq!(stream.position(), 0);
}

#[kithara::test]
fn seek_updates_position() {
    let source = ScriptSource::new(ActivityWriter::new(), [], [], b"ABCDE".to_vec());
    let mut stream = Stream::<DummyType> { source };

    let pos = stream
        .seek(SeekFrom::Start(3))
        .expect("BUG: seek to a position within the test stream must succeed");

    assert_eq!(pos, 3);
    assert_eq!(stream.position(), 3);
}

#[kithara::test]
fn seek_time_anchor_does_not_move_position() {
    let mut source = ScriptSource::new(ActivityWriter::new(), [], [], b"ABCDE".to_vec());
    source.set_position(11);
    source.anchor = Some(SourceSeekAnchor {
        byte_offset: 3,
        segment_start: Duration::from_secs(8),
        segment_end: Some(Duration::from_secs(12)),
        segment_index: Some(2),
        variant_index: Some(1),
    });
    let mut stream = Stream::<DummyType> { source };

    let anchor = stream
        .seek_time_anchor(Duration::from_millis(8_500))
        .expect("BUG: anchor resolution must succeed for the constructed test stream")
        .expect("BUG: stream must return the resolved anchor in this test");

    assert_eq!(anchor.byte_offset, 3);
    assert_eq!(
        stream.position(),
        11,
        "anchor resolution must not eagerly commit stream position"
    );
}

fn probe_wait<T: StreamType>(
    stream: &mut Stream<T>,
    range: Range<u64>,
) -> StreamResult<WaitOutcome> {
    stream.source.wait_range(range, Some(Duration::ZERO))
}
