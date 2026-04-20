#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    io::{self, Error as IoError, Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use kithara_platform::{time::Duration, tokio::runtime::Runtime};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    NullStreamContext, ReadOutcome, Source, SourcePhase, Stream, StreamContext, StreamResult,
    StreamType, Timeline,
};
use kithara_test_utils::kithara;

struct TimelineSource {
    timeline: Timeline,
    data: Vec<u8>,
}

impl TimelineSource {
    fn new(data: Vec<u8>, timeline: Timeline) -> Self {
        Self { timeline, data }
    }
}

impl Source for TimelineSource {
    type Error = io::Error;

    fn timeline(&self) -> Timeline {
        self.timeline.clone()
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        timeout: Duration,
    ) -> StreamResult<WaitOutcome, Self::Error> {
        let _ = timeout;
        Ok(WaitOutcome::Ready)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome, Self::Error> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(ReadOutcome::Data(0));
        };
        if start >= self.data.len() {
            return Ok(ReadOutcome::Data(0));
        }
        let available = &self.data[start..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(ReadOutcome::Data(n))
    }

    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Ready
    }

    fn len(&self) -> Option<u64> {
        u64::try_from(self.data.len()).ok()
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
    type Error = io::Error;
    type Events = ();

    async fn create(config: Self::Config) -> Result<Self::Source, Self::Error> {
        config
            .source
            .ok_or_else(|| IoError::other("missing source"))
    }

    fn build_stream_context(_source: &Self::Source, timeline: Timeline) -> Arc<dyn StreamContext> {
        Arc::new(NullStreamContext::new(timeline))
    }
}

#[kithara::test]
fn stream_must_use_source_timeline_as_single_position_truth() {
    let timeline = Timeline::new();
    let config = TimelineConfig {
        source: Some(TimelineSource::new(vec![1, 2, 3, 4], timeline.clone())),
    };

    let mut stream = Runtime::new()
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

    let pos = stream.seek(SeekFrom::Start(1)).expect("seek");
    assert_eq!(pos, 1);
    assert_eq!(timeline.byte_position(), 1);
}

/// Reads must succeed while the timeline is flushing.
///
/// During `apply_pending_seek()`, the decoder calls `symphonia.seek()` which
/// reads data through `Stream::read()`. At this point `flushing == true`
/// (set by `initiate_seek()`, cleared by `complete_seek()` at the end).
/// If `read()` blocks on flushing, the seek deadlocks.
#[kithara::test]
fn read_must_succeed_while_flushing() {
    let data: Vec<u8> = (0..=255).collect();
    let timeline = Timeline::new();
    let config = TimelineConfig {
        source: Some(TimelineSource::new(data, timeline.clone())),
    };

    let mut stream = Runtime::new()
        .expect("runtime")
        .block_on(Stream::<TimelineStream>::new(config))
        .expect("stream");

    // Simulate FLUSH_START — flushing is now true
    let _epoch = timeline.initiate_seek(Duration::from_secs(10));
    assert!(
        timeline.is_flushing(),
        "flushing must be set after initiate_seek"
    );

    // Decoder seeks to byte offset 100 (inside apply_pending_seek)
    let pos = stream
        .seek(SeekFrom::Start(100))
        .expect("seek must succeed while flushing");
    assert_eq!(pos, 100);

    // Decoder reads data at the new position — must NOT return Interrupted
    let mut buf = [0u8; 4];
    let n = stream
        .read(&mut buf)
        .expect("read must succeed while flushing");
    assert!(n > 0, "read must return data, got 0 bytes");
    assert_eq!(buf[0], 100, "first byte must be data at offset 100");
}
