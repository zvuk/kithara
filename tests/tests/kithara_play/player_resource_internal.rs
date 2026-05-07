//! Smoke tests for `PlayerResource` — the RT-safe wrapper around
//! `Resource` with internal scratch buffers used by the audio thread.
//! Drives `read` / `seek` / `duration` / `frames_until_eof` against
//! `TestPcmReader` and two minimal local fixtures.

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test fixture values are small positive integers/floats"
)]

use std::sync::Arc;

use kithara_audio::{DecodeError, PcmReader, PendingReason, ReadOutcome, SeekOutcome};
use kithara_bufpool::PcmPool;
use kithara_decode::{PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_integration_tests::audio_mock::TestPcmReader;
use kithara_platform::time::Duration;
use kithara_play::{
    Resource,
    impls::player_resource::{PlayerResource, ReadOutcome as BlockReadOutcome},
};
use kithara_test_utils::kithara;

fn mock_spec() -> PcmSpec {
    PcmSpec {
        channels: 2,
        sample_rate: 44100,
    }
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
        let total_frames = (seconds * spec.sample_rate as f64) as u64;
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
        let frame = (position.as_secs_f64() * self.spec.sample_rate as f64) as u64;
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
        Duration::from_secs_f64(self.frame_idx as f64 / self.spec.sample_rate as f64)
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(
            self.total_frames as f64 / self.spec.sample_rate as f64,
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
    assert!(matches!(result, BlockReadOutcome::Full));
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

    // Read 128 frames; scratch holds samples reflecting frame indices 0..N.
    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let _ = pr.read(&mut output, 0..128);
    }
    // Pre-seek samples encode early frame indices.
    assert!(left[0] < 1024.0, "pre-seek sample should be near frame 0");

    // Seek to 0.5s (frame 22050).
    pr.seek(0.5);

    // Post-seek read must reflect new position, NOT cached pre-seek samples.
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
    assert!(matches!(result, BlockReadOutcome::Full));
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
            matches!(result, BlockReadOutcome::Full),
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

    assert!(matches!(result, BlockReadOutcome::Full));
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
            BlockReadOutcome::Full | BlockReadOutcome::Partial(_) => {}
            BlockReadOutcome::Eof => break,
        }
    }

    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096);
    assert!(matches!(result, BlockReadOutcome::Eof));
}
