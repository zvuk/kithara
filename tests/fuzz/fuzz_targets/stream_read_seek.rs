#![no_main]

use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use arbitrary::{Arbitrary, Unstructured};
use kithara_platform::{time::Duration, tokio::runtime::Builder};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    DemandSlot, NullStreamContext, ReadOutcome, Source, SourcePhase, Stream, StreamContext,
    StreamResult, StreamType, Timeline, TransferCoordination,
};
use libfuzzer_sys::fuzz_target;

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

#[derive(Default)]
struct ScriptSource {
    coord: ScriptCoord,
    data: Vec<u8>,
    reads: VecDeque<ReadOutcome>,
    waits: VecDeque<WaitOutcome>,
}

#[derive(Default)]
struct ScriptCoord {
    demand: DemandSlot<()>,
    timeline: Timeline,
}

impl TransferCoordination<()> for ScriptCoord {
    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn demand(&self) -> &DemandSlot<()> {
        &self.demand
    }
}

impl Source for ScriptSource {
    type Error = io::Error;
    type Topology = ();
    type Layout = ();
    type Coord = ScriptCoord;
    type Demand = ();

    fn topology(&self) -> &Self::Topology {
        &()
    }

    fn layout(&self) -> &Self::Layout {
        &()
    }

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(self.waits.pop_front().unwrap_or(WaitOutcome::Ready))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let outcome = self.reads.pop_front().unwrap_or(ReadOutcome::Data(0));
        match outcome {
            ReadOutcome::Data(n) => {
                let start = offset as usize;
                let end = start.saturating_add(n).min(self.data.len());
                let copied = end.saturating_sub(start).min(buf.len());
                if copied > 0 {
                    buf[..copied].copy_from_slice(&self.data[start..start + copied]);
                }
                Ok(ReadOutcome::Data(copied))
            }
            other => Ok(other),
        }
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }
}

struct DummyType;

impl StreamType for DummyType {
    type Config = ScriptSource;
    type Coord = ScriptCoord;
    type Demand = ();
    type Error = io::Error;
    type Events = ();
    type Layout = ();
    type Source = ScriptSource;
    type Topology = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        Ok(config)
    }

    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
    }
}

fuzz_target!(|input: Input| {
    let waits = input.waits.into_iter().map(|w| match w {
        WaitStep::Ready => WaitOutcome::Ready,
        WaitStep::Eof => WaitOutcome::Eof,
        WaitStep::Interrupted => WaitOutcome::Interrupted,
    });
    let reads = input.reads.into_iter().map(|r| match r {
        ReadStep::Data(n) => ReadOutcome::Data(usize::from(n)),
        ReadStep::Retry => ReadOutcome::Retry,
        ReadStep::VariantChange => ReadOutcome::VariantChange,
    });

    let timeline = Timeline::new();
    let source = ScriptSource {
        coord: ScriptCoord {
            demand: DemandSlot::new(),
            timeline: timeline.clone(),
        },
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
