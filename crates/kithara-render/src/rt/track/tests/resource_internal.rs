#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara_audio::{AudioReadError, AudioSource, Fetch, SeekOutcome, TrackStep, WaitingReason};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioSpec, SegmentId};
use kithara_test_fixtures::integration_fixtures::constant_half;
use kithara_test_utils::kithara;
use kithara_worker::Task;
use num_traits::ToPrimitive;

use super::{PlayerResource, ReadOutcome as BlockReadOutcome};
use crate::{
    rt::track::PcmConsumer,
    test_pools::pools,
    worker::{
        PcmPacket,
        packet_tests::{PacketRing, chunk},
    },
};

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"))
}

fn pcm_samples(input: &'static [u8], frames: usize) -> Vec<f32> {
    input
        .chunks_exact(4)
        .cycle()
        .take(frames * 2)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("sample bytes")))
        .collect()
}

fn resource(src: &str, seconds: f64, samples: &[f32], ended: bool) -> (PlayerResource, PacketRing) {
    let mut packets = PacketRing::new(spec(), Duration::from_secs_f64(seconds), 4);
    if !samples.is_empty() {
        let mut packet = chunk(spec(), SegmentId::FIRST, 0, 0, samples);
        packet.meta.end_of_track = ended;
        packets.push(PcmPacket::Chunk(Box::new(packet)));
    }
    if ended {
        let frames = u64::try_from(samples.len() / 2).expect("frames");
        let mut end = chunk(spec(), SegmentId::FIRST, frames, frames, &[]);
        end.meta.end_of_track = true;
        packets.push(PcmPacket::Chunk(Box::new(end)));
    }
    let resource = PlayerResource::new(
        PcmConsumer::new(packets.receiver.take().expect("receiver")),
        Arc::from(src),
        &pools(),
    )
    .expect("resource fits the pool budget");
    (resource, packets)
}

fn make_player_resource(input: &'static [u8], seconds: f64) -> (PlayerResource, PacketRing) {
    let frames = (seconds * 44100.0)
        .floor()
        .to_usize()
        .expect("fixture frame count fits usize");
    resource("test.mp3", seconds, &pcm_samples(input, frames), true)
}

#[kithara::test(tokio)]
async fn duration_reflects_underlying_reader(constant_half: &'static [u8]) {
    let (pr, _packets) = make_player_resource(constant_half, 1.0);
    assert!((pr.duration() - 1.0).abs() < 0.01);
}

#[kithara::test(tokio)]
async fn read_returns_constant_samples_full(constant_half: &'static [u8]) {
    let (mut pr, _packets) = make_player_resource(constant_half, 1.0);
    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..128, &mut 8);
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
    const CALLBACK_FRAMES: usize = 512;

    let (emitted, counts) = std::sync::mpsc::channel();
    let source = RefillSource {
        emitted: 0,
        counts: emitted,
    };
    let (mut node, receiver, _lane) = crate::worker::terminal_node(source, spec(), false);
    for _ in 0..16 {
        node.tick();
    }
    let mut player =
        PlayerResource::new(PcmConsumer::new(receiver), Arc::from("chunked"), &pools())
            .expect("worker resource");
    let mut left = vec![0.0f32; CALLBACK_FRAMES];
    let mut right = vec![0.0f32; CALLBACK_FRAMES];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];

    let result = player.read(&mut output, 0..CALLBACK_FRAMES, &mut 32);

    assert_eq!(
        result,
        BlockReadOutcome::Full {
            frames: CALLBACK_FRAMES
        }
    );
    assert_eq!(
        counts.try_iter().sum::<u64>(),
        (2 * CALLBACK_FRAMES) as u64,
        "a successful callback must refill while one callback is still buffered",
    );
    assert_eq!(
        player.read(&mut output, 0..CALLBACK_FRAMES, &mut 32),
        BlockReadOutcome::Full {
            frames: CALLBACK_FRAMES
        }
    );
}

struct RefillSource {
    emitted: u64,
    counts: std::sync::mpsc::Sender<u64>,
}

impl AudioSource for RefillSource {
    type Chunk = kithara_signal::AudioChunk;

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        Some(spec().sample_rate)
    }

    fn step_track(&mut self) -> TrackStep<Self::Chunk> {
        if self.emitted == 1024 {
            return TrackStep::Blocked(WaitingReason::WaitingDemand);
        }
        let packet = chunk(
            spec(),
            SegmentId::FIRST,
            self.emitted,
            self.emitted,
            &[0.5; 1024],
        );
        self.emitted += 512;
        self.counts.send(512).expect("source read counter");
        TrackStep::Produced(Fetch::Data {
            data: packet,
            source_end: None,
        })
    }

    fn seek(&mut self, to: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target: to,
            landed_at: to,
        })
    }
}

#[kithara::test(tokio)]
async fn reset_for_seek_drops_buffered_samples() {
    let samples: Vec<f32> = (0_u16..1024)
        .flat_map(|frame| [f32::from(frame); 2])
        .collect();
    let (mut pr, mut packets) = resource("position.mp3", 1.0, &samples, false);
    let mut left = vec![0.0f32; 128];
    let mut right = vec![0.0f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let _ = pr.read(&mut output, 0..128, &mut 8);
    }
    assert!(left[0] < 1024.0, "pre-seek sample should be near frame 0");
    let buffered_before = left[127];

    let segment = SegmentId::FIRST.next();
    let samples: Vec<f32> = (1024_u16..2048)
        .flat_map(|frame| [f32::from(frame); 2])
        .collect();
    packets.push(PcmPacket::Chunk(Box::new(chunk(
        spec(),
        segment,
        0,
        1024,
        &samples,
    ))));
    pr.select_segment(segment);

    let mut left2 = vec![0.0f32; 128];
    let mut right2 = vec![0.0f32; 128];
    let mut output2: Vec<&mut [f32]> = vec![&mut left2, &mut right2];
    let _ = pr.read(&mut output2, 0..128, &mut 8);
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
    let (mut pr, _packets) = resource("pending", 1.0, &[], false);
    let mut left = vec![0.999f32; 128];
    let mut right = vec![0.999f32; 128];
    {
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let result = pr.read(&mut output, 0..128, &mut 8);
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
async fn full_read_prefetches_buffered_eof(constant_half: &'static [u8]) {
    let samples = pcm_samples(constant_half, 900);
    let (mut pr, _packets) = resource("short.mp3", 900.0 / 44100.0, &samples, true);
    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..512, &mut 8);

    assert!(matches!(result, BlockReadOutcome::Full { frames: 512 }));
    let remaining = match pr.packet.as_ref() {
        Some(PcmPacket::Chunk(chunk)) if chunk.meta.end_of_track => chunk.frames() - pr.offset,
        _ => panic!("EOF must be known from the prepared terminal packet"),
    };
    assert!(remaining > 0);
    assert!(remaining < 512);
}

#[kithara::test(tokio)]
async fn read_returns_partial_when_eof_inside_buffer(constant_half: &'static [u8]) {
    let (mut pr, _packets) = make_player_resource(constant_half, 0.01);
    let mut left = vec![0.0f32; 4096];
    let mut right = vec![0.0f32; 4096];
    let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result = pr.read(&mut output, 0..4096, &mut 8);

    let frames = match result {
        BlockReadOutcome::Partial { frames, .. } => frames,
        other => panic!("expected Partial outcome, got {other:?}"),
    };
    assert!(frames > 0);
    assert!(frames < 4096);

    let mut output2: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result2 = pr.read(&mut output2, 0..4096, &mut 8);
    assert!(matches!(result2, BlockReadOutcome::Eof));

    let mut output3: Vec<&mut [f32]> = vec![&mut left, &mut right];
    let result3 = pr.read(&mut output3, 0..4096, &mut 8);
    assert!(matches!(result3, BlockReadOutcome::Eof));
}
