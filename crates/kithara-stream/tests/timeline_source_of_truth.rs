#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek},
    ops::Range,
    sync::Arc,
};

use kithara_storage::WaitOutcome;
use kithara_stream::{
    NullStreamContext, Source, Stream, StreamContext, StreamResult, StreamType, Timeline,
};

struct TimelineSource {
    data: Vec<u8>,
    timeline: Timeline,
}

impl TimelineSource {
    fn new(data: Vec<u8>, timeline: Timeline) -> Self {
        Self { data, timeline }
    }
}

impl Source for TimelineSource {
    type Error = std::io::Error;

    fn wait_range(&mut self, _range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error> {
        let start = offset as usize;
        if start >= self.data.len() {
            return Ok(0);
        }
        let available = &self.data[start..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.timeline.set_byte_position(offset + n as u64);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }
}

#[derive(Default)]
struct TimelineConfig {
    source: Option<TimelineSource>,
}

struct TimelineStream;

impl StreamType for TimelineStream {
    type Config = TimelineConfig;
    type Source = TimelineSource;
    type Error = std::io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| std::io::Error::other("missing source"))
    }

    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
    }
}

#[test]
fn stream_must_use_source_timeline_as_single_position_truth() {
    let timeline = Timeline::new();
    let config = TimelineConfig {
        source: Some(TimelineSource::new(vec![1, 2, 3, 4], timeline.clone())),
    };

    let mut stream = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(Stream::<TimelineStream>::new(config))
        .expect("stream");

    timeline.set_byte_position(3);
    assert_eq!(
        stream.position(),
        3,
        "Stream position must mirror source timeline"
    );

    let mut out = [0u8; 2];
    let n = stream.read(&mut out).expect("read");
    assert_eq!(n, 1);
    assert_eq!(stream.position(), 4);
    assert_eq!(timeline.byte_position(), 4);

    let pos = stream.seek(std::io::SeekFrom::Start(1)).expect("seek");
    assert_eq!(pos, 1);
    assert_eq!(timeline.byte_position(), 1);
}
