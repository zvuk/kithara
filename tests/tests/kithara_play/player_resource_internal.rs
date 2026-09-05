#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::sync::atomic::{AtomicU64, Ordering};

use kithara::{
    self,
    audio::{AudioControl, AudioRead, AudioSession, DecodeError, ReadOutcome, SeekOutcome},
    decode::TrackMetadata,
    events::EventBus,
    platform::{sync::Arc, time::Duration},
    play::{
        Resource,
        bridge::RtMetrics,
        rt::track::{PlayerResource, ReadOutcome as BlockReadOutcome},
    },
    signal::AudioSpec,
};
use kithara_integration_tests::{
    audio_mock::{Fault, MockReader, TestPcmReader},
    test_defaults::Consts,
};

use crate::bufpool_ext::pools;

fn make_player_resource(seconds: f64) -> PlayerResource {
    let reader = TestPcmReader::new(Consts::AUDIO_SPEC, seconds);
    let resource = Resource::from_reader(reader, None);
    PlayerResource::new(resource, Arc::from("test.mp3"), &pools())
        .expect("player resource fits the test pool budget")
}

fn fill_planar(output: &mut [&mut [f32]], frames: usize, mut sample: impl FnMut(usize) -> f32) {
    for frame in 0..frames {
        let value = sample(frame);
        for channel in output.iter_mut() {
            channel[frame] = value;
        }
    }
}

struct ChunkReader {
    bus: EventBus,
    emitted: Arc<AtomicU64>,
    meta: TrackMetadata,
    spec: AudioSpec,
}

impl ChunkReader {
    const CHUNK_FRAMES: usize = 1_024;

    fn new(emitted: Arc<AtomicU64>) -> Self {
        Self {
            bus: EventBus::default(),
            emitted,
            meta: TrackMetadata::default(),
            spec: Consts::AUDIO_SPEC,
        }
    }

    fn emit(&self, frames: usize) -> ReadOutcome {
        self.emitted.fetch_add(frames as u64, Ordering::Relaxed);
        ReadOutcome::Frames {
            count: std::num::NonZeroUsize::new(frames).expect("chunk is non-empty"),
            position: self.position(),
            source_span: None,
        }
    }
}

impl AudioRead for ChunkReader {
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let frames = Self::CHUNK_FRAMES.min(buf.len() / usize::from(self.spec.channels));
        buf[..frames * usize::from(self.spec.channels)].fill(0.5);
        Ok(self.emit(frames))
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = Self::CHUNK_FRAMES.min(output[0].len());
        fill_planar(output, frames, |_| 0.5);
        Ok(self.emit(frames))
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn position(&self) -> Duration {
        Duration::from_secs_f64(
            self.emitted.load(Ordering::Relaxed) as f64 / f64::from(self.spec.sample_rate.get()),
        )
    }
}

impl AudioSession for ChunkReader {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.meta
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}

impl AudioControl for ChunkReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }
}

/// Reader where each frame's sample value equals the current frame index.
/// Used to observe that `seek()` clears stale buffered samples.
struct PositionReader {
    bus: EventBus,
    meta: TrackMetadata,
    spec: AudioSpec,
    frame_idx: u64,
    total_frames: u64,
}

impl PositionReader {
    fn new(seconds: f64) -> Self {
        let spec = Consts::AUDIO_SPEC;
        let total_frames = (seconds * spec.sample_rate.get() as f64) as u64;
        Self {
            bus: EventBus::default(),
            meta: TrackMetadata::default(),
            spec,
            frame_idx: 0,
            total_frames,
        }
    }

    fn read_with(&mut self, frames: usize, write: impl FnOnce(u64, usize)) -> ReadOutcome {
        let avail = (self.total_frames - self.frame_idx).min(frames as u64) as usize;
        if avail == 0 {
            return ReadOutcome::Eof {
                position: self.position(),
            };
        }
        write(self.frame_idx, avail);
        self.frame_idx += avail as u64;
        ReadOutcome::Frames {
            count: std::num::NonZeroUsize::new(avail).expect("available frames are non-zero"),
            position: self.position(),
            source_span: None,
        }
    }
}

impl AudioRead for PositionReader {
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let channels = usize::from(self.spec.channels);
        let frames = buf.len() / channels;
        Ok(self.read_with(frames, |start, avail| {
            for (frame, output) in buf.chunks_exact_mut(channels).take(avail).enumerate() {
                output.fill((start + frame as u64) as f32);
            }
        }))
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = output[0].len();
        Ok(self.read_with(frames, |start, avail| {
            fill_planar(output, avail, |frame| (start + frame as u64) as f32);
        }))
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn position(&self) -> Duration {
        Duration::from_secs_f64(self.frame_idx as f64 / self.spec.sample_rate.get() as f64)
    }
}

impl AudioSession for PositionReader {
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

impl AudioControl for PositionReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let frame = (position.as_secs_f64() * self.spec.sample_rate.get() as f64) as u64;
        self.frame_idx = frame.min(self.total_frames);
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
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
    let result = pr.read(&mut output, 0..128, &RtMetrics::default());
    assert!(matches!(result, BlockReadOutcome::Full { frames: 128 }));
    for &s in &left[..128] {
        assert!((s - 0.5).abs() < f32::EPSILON);
    }
    for &s in &right[..128] {
        assert!((s - 0.5).abs() < f32::EPSILON);
    }
}

#[kithara::test]
fn full_read_refills_before_the_next_callback_drains_scratch() {
    let emitted = Arc::new(AtomicU64::new(0));
    let reader = ChunkReader::new(Arc::clone(&emitted));
    let resource = Resource::from_reader(reader, None);
    let mut player = PlayerResource::new(resource, Arc::from("chunked"), &pools())
        .expect("player resource fits the test pool budget");
    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];

    let result = player.read(&mut output, 0..512, &RtMetrics::default());

    assert_eq!(result, BlockReadOutcome::Full { frames: 512 });
    assert_eq!(
        emitted.load(Ordering::Relaxed),
        2 * ChunkReader::CHUNK_FRAMES as u64,
        "a successful callback must refill while one callback is still buffered",
    );
}

#[kithara::test(tokio)]
async fn reset_for_seek_drops_buffered_samples() {
    let reader = PositionReader::new(1.0);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("position.mp3"), &pools())
        .expect("player resource fits the test pool budget");

    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let _ = pr.read(&mut output, 0..128, &RtMetrics::default());
    }
    assert!(left[0] < 1024.0, "pre-seek sample should be near frame 0");
    let buffered_before = left[127];

    pr.reset_for_seek();

    let mut left2 = vec![0.0f32; 128];
    let mut right2 = vec![0.0f32; 128];
    let mut output2: Vec<&mut [f32]> = vec![&mut left2, &mut right2];
    let _ = pr.read(&mut output2, 0..128, &RtMetrics::default());
    assert!(
        left2[0] > buffered_before,
        "the reset must discard the scratch and pull fresh frames, got {}",
        left2[0]
    );
}

/// When the reader returns 0 frames and is NOT at EOF (e.g. async seek
/// in progress), `read()` must zero-fill the output buffers. Otherwise
/// the caller's stale samples from the previous audio-thread cycle leak
/// through, heard as a looped/glitched frame during seek.
#[kithara::test(tokio)]
async fn read_zeroes_output_when_no_data_available() {
    let reader = MockReader::faulty(Consts::AUDIO_SPEC, Fault::Stall);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("pending"), &pools())
        .expect("player resource fits the test pool budget");

    let mut left = vec![0.999f32; 128];
    let mut right = vec![0.999f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let result = pr.read(&mut output, 0..128, &RtMetrics::default());
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
    let reader = TestPcmReader::new(Consts::AUDIO_SPEC, 900.0 / 44100.0);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"), &pools())
        .expect("player resource fits the test pool budget");

    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..512, &RtMetrics::default());

    assert!(matches!(result, BlockReadOutcome::Full { frames: 512 }));
    let remaining = pr
        .frames_until_eof()
        .expect("BUG: EOF should be known after prefetch");
    assert!(remaining > 0);
    assert!(remaining < 512);
}

#[kithara::test(tokio)]
async fn read_returns_partial_when_eof_inside_buffer() {
    let reader = TestPcmReader::new(Consts::AUDIO_SPEC, 0.01);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"), &pools())
        .expect("player resource fits the test pool budget");

    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096, &RtMetrics::default());

    let frames = match result {
        BlockReadOutcome::Partial { frames, .. } => frames,
        other => panic!("expected Partial outcome, got {other:?}"),
    };
    assert!(frames > 0);
    assert!(frames < 4096);

    let mut output2: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result2 = pr.read(&mut output2, 0..4096, &RtMetrics::default());
    assert!(matches!(result2, BlockReadOutcome::Eof));

    let mut output3: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result3 = pr.read(&mut output3, 0..4096, &RtMetrics::default());
    assert!(matches!(result3, BlockReadOutcome::Eof));
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
    let reader = MockReader::faulty(Consts::AUDIO_SPEC, Fault::DecodeError);
    let resource = Resource::from_reader(reader, None);
    let mut pr = PlayerResource::new(resource, Arc::from("failing.mp3"), &pools())
        .expect("player resource fits the test pool budget");

    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096, &RtMetrics::default());

    match result {
        BlockReadOutcome::Failed => {}
        BlockReadOutcome::Eof | BlockReadOutcome::Partial { .. } => panic!(
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
