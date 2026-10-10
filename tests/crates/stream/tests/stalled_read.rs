#![cfg(feature = "flash")]

use std::{
    io::{Error as IoError, Read},
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
};

use futures::executor::block_on;
use kithara::{
    events::EventBus,
    platform::{
        sync::{Arc, ThreadGate, WaitGate},
        time::Duration,
    },
    storage::WaitOutcome,
    stream::{
        Activity, ActivityWriter, PlayheadRead, PlayheadWrite, ReadOutcome, Source, SourceError,
        SourcePhase, SourceProbe, Stream, StreamError, StreamResult, StreamType,
    },
};
use kithara_integration_tests::memory_source::MemorySource;

/// A reader parked on a range nobody will fetch: every blocking wait sits out
/// its re-aim interval and reports the budget spent, and no bytes ever arrive.
struct StalledSource {
    bytes: MemorySource,
    gate: ThreadGate,
}

impl StalledSource {
    const REAIM: Duration = Duration::from_millis(25);
}

impl Source for StalledSource {
    fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
        SourcePhase::Waiting
    }

    fn wait_range(
        &mut self,
        _range: Range<u64>,
        _timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        let since = self.gate.current();
        self.gate.wait_timeout(since, Self::REAIM);
        Err(StreamError::Source(SourceError::WaitBudgetExceeded))
    }

    delegate::delegate! {
        to self.bytes {
            fn activity(&self) -> Activity;
            fn take_activity_writer(&mut self) -> Option<ActivityWriter>;
            fn advance(&self, n: u64);
            fn len(&self) -> Option<u64>;
            fn playhead_read(&self) -> Arc<dyn PlayheadRead>;
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            fn position(&self) -> u64;
            fn probe(&self) -> Arc<dyn SourceProbe>;
            fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;
            fn set_position(&self, pos: u64);
        }
    }
}

struct StalledStream;

impl StreamType for StalledStream {
    type Config = Option<StalledSource>;
    type Events = EventBus;
    type Source = StalledSource;

    async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
        config.ok_or_else(|| SourceError::other(IoError::other("no source")))
    }
}

/// Each blocking wait returns within its re-aim interval, so only the read as a
/// whole can tell that nothing arrives: it must trip the hang watchdog instead of
/// re-aiming forever. The watchdog budget is virtual under flash.
#[kithara::test(timeout(Duration::from_secs(10)))]
fn a_read_that_never_progresses_trips_the_hang_watchdog() {
    let source = StalledSource {
        bytes: MemorySource::new(b"never delivered".to_vec()),
        gate: ThreadGate::default(),
    };
    let mut stream =
        block_on(Stream::<StalledStream>::new(Some(source))).expect("the source is configured");
    let mut buf = [0_u8; 8];

    let payload = catch_unwind(AssertUnwindSafe(|| stream.read(&mut buf)))
        .expect_err("a read with no progress must trip the watchdog");
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .expect("the watchdog panics with a message");

    assert!(
        message.contains("HangDetector") && message.contains("stream::read"),
        "the blocking read's own watchdog must fire: {message}"
    );
}
