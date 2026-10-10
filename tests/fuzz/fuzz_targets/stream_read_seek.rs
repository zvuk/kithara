#![no_main]

use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use arbitrary::{Arbitrary, Unstructured};
use kithara::{
    platform::{time::Duration, tokio::runtime::Builder},
    storage::WaitOutcome,
    stream::{
        Activity, ActivityWriter, ByteMap, PlayheadRead, PlayheadState, PlayheadWrite, ReadOutcome,
        Source, SourcePhase, SourceProbe, Stream, StreamResult, StreamType,
    },
};
use libfuzzer_sys::fuzz_target;

/// Always-ready byte-space probe sharing the fuzz source's cursor and length.
struct ReadyProbe {
    len: u64,
    position: Arc<AtomicU64>,
}

impl SourceProbe for ReadyProbe {
    fn phase(&self) -> SourcePhase {
        SourcePhase::Ready
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn len(&self) -> Option<u64> {
        Some(self.len)
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        None
    }
}

#[derive(Debug)]
enum WaitStep {
    Ready,
    Eof,
    Interrupted,
}

#[derive(Debug)]
enum ReadStep {
    Data(u8),
    Retry,
    VariantChange,
}

#[derive(Debug)]
enum Op {
    Read(u8),
    SeekStart(u16),
    SeekCurrent(i16),
    SeekEnd(i16),
}

#[derive(Debug)]
struct Input {
    waits: Vec<WaitStep>,
    reads: Vec<ReadStep>,
    ops: Vec<Op>,
    data: Vec<u8>,
}

impl<'a> Arbitrary<'a> for WaitStep {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range::<u8>(0..=2)? {
            0 => Self::Ready,
            1 => Self::Eof,
            _ => Self::Interrupted,
        })
    }
}

impl<'a> Arbitrary<'a> for ReadStep {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range::<u8>(0..=2)? {
            0 => Self::Data(u.int_in_range::<u8>(0..=64)?),
            1 => Self::Retry,
            _ => Self::VariantChange,
        })
    }
}

impl<'a> Arbitrary<'a> for Op {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(match u.int_in_range::<u8>(0..=3)? {
            0 => Self::Read(u.int_in_range::<u8>(0..=64)?),
            1 => Self::SeekStart(u.int_in_range::<u16>(0..=2048)?),
            2 => Self::SeekCurrent(u.int_in_range::<i16>(-256..=256)?),
            _ => Self::SeekEnd(u.int_in_range::<i16>(-256..=256)?),
        })
    }
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut waits = u.arbitrary::<Vec<WaitStep>>()?;
        waits.truncate(128);
        let mut reads = u.arbitrary::<Vec<ReadStep>>()?;
        reads.truncate(128);
        let mut ops = u.arbitrary::<Vec<Op>>()?;
        ops.truncate(128);
        let mut data = u.arbitrary::<Vec<u8>>()?;
        data.truncate(2048);
        Ok(Self {
            waits,
            reads,
            ops,
            data,
        })
    }
}

struct ScriptSource {
    activity: Activity,
    activity_writer: Option<ActivityWriter>,
    playhead: Arc<PlayheadState>,
    position: Arc<AtomicU64>,
    data: Vec<u8>,
    reads: VecDeque<ReadOutcome>,
    waits: VecDeque<WaitOutcome>,
}

impl Source for ScriptSource {
    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
    }

    fn activity(&self) -> Activity {
        self.activity.clone()
    }

    fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
        self.activity_writer.take()
    }

    fn position(&self) -> u64 {
        self.position.load(Ordering::Acquire)
    }

    fn advance(&self, n: u64) {
        self.position.fetch_add(n, Ordering::AcqRel);
    }

    fn set_position(&self, pos: u64) {
        self.position.store(pos, Ordering::Release);
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        Ok(self.waits.pop_front().unwrap_or(WaitOutcome::Ready))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        let outcome = self.reads.pop_front().unwrap_or(ReadOutcome::Eof);
        match outcome {
            ReadOutcome::Bytes(count) => {
                let start = offset as usize;
                let end = start.saturating_add(count.get()).min(self.data.len());
                let copied = end.saturating_sub(start).min(buf.len());
                let Some(nz) = std::num::NonZeroUsize::new(copied) else {
                    return Ok(ReadOutcome::Eof);
                };
                buf[..copied].copy_from_slice(&self.data[start..start + copied]);
                Ok(ReadOutcome::Bytes(nz))
            }
            other => Ok(other),
        }
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(ReadyProbe {
            len: self.data.len() as u64,
            position: Arc::clone(&self.position),
        })
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

struct DummyType;

impl StreamType for DummyType {
    type Config = ScriptSource;
    type Events = ();
    type Source = ScriptSource;

    async fn create(config: Self::Config) -> Result<Self::Source, kithara::stream::SourceError> {
        Ok(config)
    }
}

fuzz_target!(|input: Input| {
    let waits = input.waits.into_iter().map(|w| match w {
        WaitStep::Ready => WaitOutcome::Ready,
        WaitStep::Eof => WaitOutcome::Eof,
        WaitStep::Interrupted => WaitOutcome::Interrupted,
    });
    let reads = input.reads.into_iter().map(|r| match r {
        ReadStep::Data(n) => match std::num::NonZeroUsize::new(usize::from(n)) {
            Some(nz) => ReadOutcome::Bytes(nz),
            None => ReadOutcome::Eof,
        },
        ReadStep::Retry => ReadOutcome::Pending(kithara::stream::PendingReason::Retry),
        ReadStep::VariantChange => {
            ReadOutcome::Pending(kithara::stream::PendingReason::VariantChange)
        }
    });

    let activity_writer = ActivityWriter::new();
    let source = ScriptSource {
        activity: activity_writer.reader(),
        activity_writer: Some(activity_writer),
        playhead: Arc::new(PlayheadState::new()),
        position: Arc::new(AtomicU64::new(0)),
        data: input.data,
        reads: reads.collect(),
        waits: waits.collect(),
    };
    let runtime = match Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let mut stream = match runtime.block_on(Stream::<DummyType>::new(source)) {
        Ok(stream) => stream,
        Err(_) => return,
    };

    for op in input.ops {
        match op {
            Op::Read(len) => {
                let mut buf = vec![0u8; usize::from(len)];
                let _ = stream.read(&mut buf);
                let pos = stream.position();
                let total = stream.len().unwrap_or(0);
                assert!(pos <= total);
            }
            Op::SeekStart(pos) => {
                let _ = stream.seek(SeekFrom::Start(u64::from(pos)));
            }
            Op::SeekCurrent(delta) => {
                let _ = stream.seek(SeekFrom::Current(i64::from(delta)));
            }
            Op::SeekEnd(delta) => {
                let _ = stream.seek(SeekFrom::End(i64::from(delta)));
            }
        }
    }
});
