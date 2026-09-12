use std::{
    io,
    num::{NonZeroU64, NonZeroUsize},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use kithara_bufpool::testing::pools;
use kithara_encode::EncodeConfig;
use kithara_output::{LiveOutput, OutputGroup};
use kithara_platform::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use kithara_record::{
    LiveRecorder, LiveRecordingConfig, LiveRecordingError, LiveRecordingHandle,
    LiveRecordingReport, PartSinkFactory, RecordingConfig, RecordingSink,
};
use kithara_signal::AudioSpec;
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_fixtures::unit_fixtures::{record_labels, record_signed};
use kithara_test_utils::kithara;
use kithara_worker::{Worker, WorkerConfig};

#[derive(Clone, Default)]
struct Parts(Arc<Mutex<Vec<Vec<u8>>>>);

struct TestFactory {
    parts: Parts,
}

impl PartSinkFactory for TestFactory {
    type Sink = TestSink;

    fn open(&mut self, _part: u64) -> Result<Self::Sink, io::Error> {
        Ok(TestSink {
            bytes: Vec::new(),
            parts: self.parts.clone(),
        })
    }
}

struct TestSink {
    parts: Parts,
    bytes: Vec<u8>,
}

impl RecordingSink for TestSink {
    type Error = io::Error;
    type Output = ();

    fn abort(&mut self) {
        self.bytes.clear();
    }

    fn commit(&mut self, final_len: u64) -> Result<Self::Output, Self::Error> {
        let final_len = usize::try_from(final_len).map_err(io::Error::other)?;
        self.bytes.truncate(final_len);
        self.parts.0.lock().push(self.bytes.clone());
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Self::Error> {
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("test sink length overflow"))?;
        self.bytes.resize(self.bytes.len().max(end), 0);
        self.bytes[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

fn write(output: &mut impl LiveOutput, frames: &[f32]) {
    let left: Vec<_> = frames.chunks_exact(2).map(|frame| frame[0]).collect();
    let right: Vec<_> = frames.chunks_exact(2).map(|frame| frame[1]).collect();
    output.write_stereo(frames.len() / 2, &left, &right);
}

fn wav_samples(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[36..40], b"data");
    let data_len = u32::from_le_bytes(bytes[40..44].try_into().expect("WAV data length"));
    assert_eq!(bytes.len(), 44 + data_len as usize);
    bytes[44..]
        .chunks_exact(4)
        .map(|sample| f32::from_le_bytes(sample.try_into().expect("one f32 sample")))
        .collect()
}

fn wait_result(handle: &LiveRecordingHandle) -> Result<LiveRecordingReport, LiveRecordingError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = handle.finish() {
            return result;
        }
        assert!(Instant::now() < deadline, "live recorder did not finish");
        thread::yield_now();
    }
}

#[kithara::test(native, flash(false))]
fn manual_cut_and_rotation_publish_exact_independent_parts(record_labels: Vec<f32>) {
    let parts = Parts::default();
    let config = LiveRecordingConfig::builder(
        Worker::new(WorkerConfig::new()),
        pools(),
        TestFactory {
            parts: parts.clone(),
        },
    )
    .recording(RecordingConfig::builder().build())
    .buffer_frames(NonZeroUsize::new(32).expect("test buffer frames"))
    .tick_frames(NonZeroUsize::new(8).expect("test tick frames"))
    .rotation_frames(NonZeroU64::new(4).expect("test rotation frames"))
    .build();
    let (mut output, handle) = LiveRecorder::start(config).expect("start live recorder");

    write(&mut output, &record_labels[..6]);
    handle.cut();
    write(&mut output, &record_labels[6..12]);
    write(&mut output, &record_labels[12..18]);
    let report = wait_result(&handle).expect("finish live recorder");

    assert_eq!(report.frames, 9);
    assert_eq!(report.parts, 3);
    let parts = parts.0.lock();
    assert_eq!(parts.len(), 3);
    assert_eq!(wav_samples(&parts[0]), [1.0, 101.0, 2.0, 102.0, 3.0, 103.0]);
    assert_eq!(
        wav_samples(&parts[1]),
        [4.0, 104.0, 5.0, 105.0, 6.0, 106.0, 7.0, 107.0]
    );
    assert_eq!(wav_samples(&parts[2]), [8.0, 108.0, 9.0, 109.0]);
}

#[derive(Clone)]
struct Lifecycle {
    aborted: Arc<AtomicBool>,
    opened: Arc<AtomicBool>,
}

struct LifecycleFactory {
    lifecycle: Lifecycle,
    fail_write: bool,
}

impl PartSinkFactory for LifecycleFactory {
    type Sink = LifecycleSink;

    fn open(&mut self, _part: u64) -> Result<Self::Sink, io::Error> {
        self.lifecycle.opened.store(true, Ordering::Release);
        Ok(LifecycleSink {
            fail_write: self.fail_write,
            lifecycle: self.lifecycle.clone(),
        })
    }
}

struct LifecycleSink {
    lifecycle: Lifecycle,
    fail_write: bool,
}

impl RecordingSink for LifecycleSink {
    type Error = io::Error;
    type Output = ();

    fn abort(&mut self) {
        self.lifecycle.aborted.store(true, Ordering::Release);
    }

    fn commit(&mut self, _final_len: u64) -> Result<Self::Output, Self::Error> {
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> Result<(), Self::Error> {
        if self.fail_write {
            return Err(io::Error::other("injected sink failure"));
        }
        Ok(())
    }
}

fn lifecycle() -> Lifecycle {
    Lifecycle {
        aborted: Arc::new(AtomicBool::new(false)),
        opened: Arc::new(AtomicBool::new(false)),
    }
}

fn wait_true(value: &AtomicBool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !value.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "{message}");
        thread::yield_now();
    }
}

#[kithara::test(native, flash(false))]
fn bounded_overflow_aborts_the_open_part(record_signed: Vec<f32>) {
    let lifecycle = lifecycle();
    let config = LiveRecordingConfig::builder(
        Worker::new(WorkerConfig::new()),
        pools(),
        LifecycleFactory {
            lifecycle: lifecycle.clone(),
            fail_write: false,
        },
    )
    .buffer_frames(NonZeroUsize::MIN)
    .tick_frames(NonZeroUsize::MIN)
    .build();
    let (mut output, handle) = LiveRecorder::start(config).expect("start live recorder");

    write(&mut output, &record_signed[..2]);
    wait_true(&lifecycle.opened, "recorder did not open its first part");
    write(&mut output, &record_signed[2..6]);

    let result = wait_result(&handle);
    assert!(
        matches!(
            result,
            Err(LiveRecordingError::BufferOverflow { buffer_frames: 1 })
        ),
        "unexpected overflow result: {result:?}"
    );
    assert!(lifecycle.aborted.load(Ordering::Acquire));
}

struct FrameProbe(Arc<AtomicUsize>);

impl LiveOutput for FrameProbe {
    fn reconfigure(&mut self, _spec: AudioSpec) {}

    fn write_stereo(&mut self, frames: usize, _left: &[f32], _right: &[f32]) {
        self.0.fetch_add(frames, Ordering::Relaxed);
    }
}

#[kithara::test(native, flash(false))]
fn sink_failure_does_not_stop_a_sibling_output(record_signed: Vec<f32>) {
    let lifecycle = lifecycle();
    let config = LiveRecordingConfig::builder(
        Worker::new(WorkerConfig::new()),
        pools(),
        LifecycleFactory {
            lifecycle: lifecycle.clone(),
            fail_write: true,
        },
    )
    .recording(
        RecordingConfig::builder()
            .encode(
                EncodeConfig::builder()
                    .sample_rate(48_000)
                    .channels(2)
                    .packet_frames(1)
                    .build(),
            )
            .build(),
    )
    .buffer_frames(NonZeroUsize::new(8).expect("test buffer frames"))
    .tick_frames(NonZeroUsize::MIN)
    .build();
    let (output, handle) = LiveRecorder::start(config).expect("start live recorder");
    let sibling_frames = Arc::new(AtomicUsize::new(0));
    let mut outputs = OutputGroup::new();
    outputs.push(output);
    outputs.push(FrameProbe(Arc::clone(&sibling_frames)));

    write(&mut outputs, &record_signed[..2]);
    assert!(matches!(
        wait_result(&handle),
        Err(LiveRecordingError::Part { part: 1, .. })
    ));
    write(&mut outputs, &record_signed[2..4]);

    assert_eq!(sibling_frames.load(Ordering::Relaxed), 2);
    assert!(lifecycle.aborted.load(Ordering::Acquire));
}
