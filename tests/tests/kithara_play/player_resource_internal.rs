#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::{num::NonZeroU32, sync::Arc};

use kithara::{
    self,
    audio::{DecodeError, PcmReader, PendingReason, ReadOutcome, SeekOutcome},
    bufpool::PcmPool,
    decode::{PcmSpec, TrackMetadata},
    events::EventBus,
    platform::time::Duration,
    play::{
        Resource,
        rt::track::{PlayerResource, ReadOutcome as BlockReadOutcome},
    },
};
use kithara_integration_tests::audio_mock::TestPcmReader;

fn mock_spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
}

fn make_player_resource(seconds: f64) -> PlayerResource {
    let reader = TestPcmReader::new(mock_spec(), seconds);
    let resource = Resource::from_reader(reader, None);
    PlayerResource::new(resource, Arc::from("test.mp3"), &PcmPool::default())
}

struct PendingReader {
    bus: EventBus,
    meta: TrackMetadata,
    spec: PcmSpec,
}

impl PendingReader {
    fn new() -> Self {
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            spec: mock_spec(),
        }
    }
}

impl PcmReader for PendingReader {
    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            reason: PendingReason::Buffering,
            position: Duration::ZERO,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            reason: PendingReason::Buffering,
            position: Duration::ZERO,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

/// Reader where each frame's sample value equals the current frame index.
/// Used to observe that `seek()` clears stale buffered samples.
struct PositionReader {
    bus: EventBus,
    meta: TrackMetadata,
    spec: PcmSpec,
    frame_idx: u64,
    total_frames: u64,
}

impl PositionReader {
    fn new(seconds: f64) -> Self {
        let spec = mock_spec();
        let total_frames = (seconds * spec.sample_rate.get() as f64) as u64;
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            spec,
            frame_idx: 0,
            total_frames,
        }
    }
}

impl PcmReader for PositionReader {
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let channels = self.spec.channels as usize;
        let frames = buf.len() / channels;
        let avail = (self.total_frames - self.frame_idx).min(frames as u64) as usize;
        if avail == 0 {
            return Ok(ReadOutcome::Eof {
                position: self.position(),
            });
        }
        for i in 0..avail {
            let v = (self.frame_idx + i as u64) as f32;
            for c in 0..channels {
                buf[i * channels + c] = v;
            }
        }
        self.frame_idx += avail as u64;
        Ok(ReadOutcome::Frames {
            count: std::num::NonZeroUsize::new(avail).unwrap(),
            position: self.position(),
        })
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = output[0].len();
        let avail = (self.total_frames - self.frame_idx).min(frames as u64) as usize;
        if avail == 0 {
            return Ok(ReadOutcome::Eof {
                position: self.position(),
            });
        }
        for i in 0..avail {
            let v = (self.frame_idx + i as u64) as f32;
            for ch in output.iter_mut() {
                ch[i] = v;
            }
        }
        self.frame_idx += avail as u64;
        Ok(ReadOutcome::Frames {
            count: std::num::NonZeroUsize::new(avail).unwrap(),
            position: self.position(),
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let frame = (position.as_secs_f64() * self.spec.sample_rate.get() as f64) as u64;
        self.frame_idx = frame.min(self.total_frames);
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn position(&self) -> Duration {
        Duration::from_secs_f64(self.frame_idx as f64 / self.spec.sample_rate.get() as f64)
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(
            self.total_frames as f64 / self.spec.sample_rate.get() as f64,
        ))
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

#[kithara::test(tokio)]
async fn duration_reflects_underlying_reader() {
    let pr = make_player_resource(1.0);
    assert!((pr.duration() - 1.0).abs() < 0.01);
}

#[kithara::test(tokio)]
async fn read_returns_constant_samples_full() {
    let mut pr = make_player_resource(1.0);
    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..128);
    assert!(matches!(result, BlockReadOutcome::Full { frames: 128 }));
    for &s in &left[..128] {
        assert!((s - 0.5).abs() < f32::EPSILON);
    }
    for &s in &right[..128] {
        assert!((s - 0.5).abs() < f32::EPSILON);
    }
}

#[kithara::test(tokio)]
async fn seek_clears_buffered_samples() {
    let reader = PositionReader::new(1.0);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("position.mp3"), &PcmPool::default());

    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let _ = pr.read(&mut output, 0..128);
    }
    assert!(left[0] < 1024.0, "pre-seek sample should be near frame 0");

    pr.seek(0.5);

    let mut left2 = vec![0.0f32; 128];
    let mut right2 = vec![0.0f32; 128];
    let mut output2: Vec<&mut [f32]> = vec![&mut left2, &mut right2];
    let _ = pr.read(&mut output2, 0..128);
    assert!(
        left2[0] > 20_000.0,
        "post-seek sample must reflect new position, got {}",
        left2[0]
    );
}

#[kithara::test(tokio)]
async fn zero_read_without_eof_is_not_error() {
    let reader = PendingReader::new();
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("pending"), &PcmPool::default());

    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..128);
    assert!(matches!(result, BlockReadOutcome::Full { frames: 0 }));
}

/// When the reader returns 0 frames and is NOT at EOF (e.g. async seek
/// in progress), `read()` must zero-fill the output buffers. Otherwise
/// the caller's stale samples from the previous audio-thread cycle leak
/// through, heard as a looped/glitched frame during seek.
#[kithara::test(tokio)]
async fn read_zeroes_output_when_no_data_available() {
    let reader = PendingReader::new();
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("pending"), &PcmPool::default());

    let mut left = vec![0.999f32; 128];
    let mut right = vec![0.999f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let result = pr.read(&mut output, 0..128);
        assert!(
            matches!(result, BlockReadOutcome::Full { frames: 0 }),
            "zero-read without EOF must not error"
        );
    }

    let max_left = left.iter().copied().fold(0.0f32, f32::max);
    let max_right = right.iter().copied().fold(0.0f32, f32::max);
    assert!(
        max_left == 0.0 && max_right == 0.0,
        "output must be silence when reader returns 0 frames, \
         but got max_left={max_left} max_right={max_right}"
    );
}

#[kithara::test(tokio)]
async fn full_read_prefetches_buffered_eof() {
    let reader = TestPcmReader::new(mock_spec(), 900.0 / 44100.0);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"), &PcmPool::default());

    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..512);

    assert!(matches!(result, BlockReadOutcome::Full { frames: 512 }));
    let remaining = pr
        .frames_until_eof()
        .expect("BUG: EOF should be known after prefetch");
    assert!(remaining > 0);
    assert!(remaining < 512);
}

#[kithara::test(tokio)]
async fn read_returns_partial_when_eof_inside_buffer() {
    let reader = TestPcmReader::new(mock_spec(), 0.01);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"), &PcmPool::default());

    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096);

    let frames = match result {
        BlockReadOutcome::Partial(frames) => frames,
        other => panic!("expected Partial outcome, got {other:?}"),
    };
    assert!(frames > 0);
    assert!(frames < 4096);

    let mut output2: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result2 = pr.read(&mut output2, 0..4096);
    assert!(matches!(result2, BlockReadOutcome::Eof));
}

/// Reader that returns a typed decode `Err` on every read — models
/// the in-the-wild scenario where the audio worker's PCM producer
/// closed mid-stream after a transient decoder failure (e.g. failed
/// seek leaves symphonia's atom reader cursor invalid, next
/// `next_chunk` returns `isomp4: no atom pending read`).
struct FailingReader {
    bus: EventBus,
    meta: TrackMetadata,
    spec: PcmSpec,
}

impl FailingReader {
    fn new() -> Self {
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            spec: mock_spec(),
        }
    }
}

impl PcmReader for FailingReader {
    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Err(DecodeError::InvalidData {
            detail: "mock: decoder failed mid-stream",
        })
    }
    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Err(DecodeError::InvalidData {
            detail: "mock: decoder failed mid-stream",
        })
    }
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
    fn spec(&self) -> PcmSpec {
        self.spec
    }
    fn position(&self) -> Duration {
        Duration::ZERO
    }
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(169))
    }
    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }
    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

/// Contract test for the user-reported "preliminary EOF" bug.
///
/// Before the fix, any `Err` from the underlying audio reader set
/// `eof_seen=true`, which made the next `read()` return `Partial(0)`
/// or `Eof`. The Player then emitted `PlaybackStopped { Eof }` and
/// the Queue auto-advanced — even though the track did NOT actually
/// reach its natural end. After the fix, `Err` sets `failed=true`
/// and `read()` returns the new `Failed` variant, so callers can
/// distinguish "track aborted mid-stream" from "track played out".
#[kithara::test(tokio)]
async fn read_returns_failed_not_eof_on_decoder_error() {
    let reader = FailingReader::new();
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("failing.mp3"), &PcmPool::default());

    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096);

    match result {
        BlockReadOutcome::Failed => {}
        BlockReadOutcome::Eof | BlockReadOutcome::Partial(_) => panic!(
            "decoder Err must NOT be conflated with natural EOF — got {result:?}; \
             this is the false-EOF bug from app.log"
        ),
        BlockReadOutcome::Full { .. } => {
            panic!("decoder Err must surface as Failed, not Full silence — got {result:?}")
        }
    }

    assert!(
        pr.frames_until_eof().is_none(),
        "frames_until_eof must NOT report an EOF after a decode failure \
         (otherwise the Queue treats it as a natural-end signal)"
    );
}

#[kithara::test(tokio)]
async fn read_returns_eof_when_already_drained() {
    let reader = TestPcmReader::new(mock_spec(), 0.01);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"), &PcmPool::default());

    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];

    loop {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        match pr.read(&mut output, 0..4096) {
            BlockReadOutcome::Full { .. } | BlockReadOutcome::Partial(_) => {}
            BlockReadOutcome::Eof => break,
            BlockReadOutcome::Failed => panic!("unexpected Failed in EOF test"),
        }
    }

    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096);
    assert!(matches!(result, BlockReadOutcome::Eof));
}
