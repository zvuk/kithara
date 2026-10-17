use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;
use kithara_test_fixtures::mock_fixtures::zero_packet;
use kithara_test_utils::kithara;

use super::{
    ComposedDecoder, DecoderRuntime,
    seek_trim_tests::{BoundaryFrame, BoundaryFrameDemuxer, test_spec, track_with_rate},
    test_counting_codec::CountingCodec,
};
use crate::{DecodeResult, Decoder, DecoderChunkOutcome, codec::FrameCodec};

pub(crate) fn head_gap_fixture(pcm: Vec<f32>, packet: &[u8]) -> Box<dyn Decoder> {
    let spec = test_spec(1_000);
    let codec = CountingCodec::new(pcm, spec, 2).with_frames([0, 2, 2]);
    let demuxer = BoundaryFrameDemuxer {
        track: track_with_rate(1_000),
        held: packet.to_vec(),
        idx: 0,
        frames: vec![
            BoundaryFrame {
                pts: Duration::from_millis(42),
                duration: Duration::from_millis(17),
            },
            BoundaryFrame {
                pts: Duration::from_millis(59),
                duration: Duration::from_millis(2),
            },
            BoundaryFrame {
                pts: Duration::from_millis(92),
                duration: Duration::from_millis(2),
            },
        ],
    };
    Box::new(ComposedDecoder::new(
        demuxer,
        codec,
        DecoderRuntime::for_test(),
    ))
}

#[kithara::test]
fn original_pcm_basis_survives_a_full_head_strip_and_a_later_pts_gap(zero_packet: &'static [u8]) {
    let expected = [0.25, -0.5];
    let mut decoder = head_gap_fixture(expected.to_vec(), zero_packet);
    assert_eq!(decoder.initial_pcm_origin(), None);
    let DecoderChunkOutcome::Chunk(first) = decoder.next_chunk().expect("original PCM") else {
        panic!("original PCM chunk");
    };
    let origin = decoder
        .initial_pcm_origin()
        .expect("original packet and PCM");
    assert_eq!(first.meta.timestamp, Duration::from_millis(59));
    assert_eq!(first.meta.frames, 2);
    assert_eq!(first.samples.as_ref(), expected.as_slice());
    assert_eq!(origin.position(), Duration::from_millis(59));
    assert_eq!(origin.comparison_base(), Duration::from_millis(42));
    assert_eq!(decoder.timeline_gap_frames(), 17);

    let DecoderChunkOutcome::Chunk(later) = decoder.next_chunk().expect("later PCM") else {
        panic!("later PCM chunk");
    };
    assert_eq!(later.meta.timestamp, Duration::from_millis(92));
    assert_eq!(later.meta.frames, 2);
    assert_eq!(later.samples.as_ref(), expected.as_slice());
    assert_eq!(decoder.timeline_gap_frames(), 31);
    assert_eq!(decoder.initial_pcm_origin(), Some(origin));
}

#[kithara::test]
fn partial_head_strip_keeps_the_original_packet_basis_before_later_pcm(zero_packet: &'static [u8]) {
    let spec = test_spec(1_000);
    let pcm: Vec<_> = (0..64_u16)
        .map(|frame| f32::from(frame) / 128.0 - 0.25)
        .collect();
    let codec = CountingCodec::new(pcm.clone(), spec, 64).with_frames([0, 13, 64]);
    let demuxer = BoundaryFrameDemuxer {
        track: track_with_rate(1_000),
        held: zero_packet.to_vec(),
        idx: 0,
        frames: vec![
            BoundaryFrame {
                pts: Duration::from_millis(42),
                duration: Duration::from_millis(10),
            },
            BoundaryFrame {
                pts: Duration::from_millis(52),
                duration: Duration::from_millis(20),
            },
            BoundaryFrame {
                pts: Duration::from_millis(103),
                duration: Duration::from_millis(64),
            },
        ],
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
    let DecoderChunkOutcome::Chunk(first) = decoder.next_chunk().expect("partial head PCM") else {
        panic!("partial head PCM chunk");
    };
    let origin = decoder.initial_pcm_origin().expect("original geometry");
    assert_eq!(first.meta.timestamp, Duration::from_millis(59));
    assert_eq!(first.meta.frames, 13);
    assert_eq!(first.samples.as_ref(), &pcm[..13]);
    assert_eq!(origin.position(), Duration::from_millis(59));
    assert_eq!(origin.comparison_base(), Duration::from_millis(42));
    assert_eq!(decoder.timeline_gap_frames(), 17);
    let DecoderChunkOutcome::Chunk(later) = decoder.next_chunk().expect("later PCM") else {
        panic!("later PCM chunk");
    };
    assert_eq!(later.meta.timestamp, Duration::from_millis(103));
    assert_eq!(later.samples.as_ref(), pcm.as_slice());
    assert_eq!(decoder.timeline_gap_frames(), 31);
    assert_eq!(decoder.initial_pcm_origin(), Some(origin));
}

struct TimestampCodec {
    spec: AudioSpec,
    pcm: Vec<f32>,
    output_pts: Duration,
    bias: u64,
}

impl FrameCodec for TimestampCodec {
    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: _bytes,
            pts: _pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        super::write_silent_test_frame(&self.pcm, self.spec, 2, out)
            .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn decoded_pts(&self) -> Option<Duration> {
        Some(self.output_pts)
    }

    fn timestamp_bias_frames(&self) -> u64 {
        self.bias
    }

    fn flush(&mut self) -> DecodeResult<()> {
        Ok(())
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

#[kithara::test]
#[case::bias_reflected_in_pcm_pts(717, 700)]
#[case::bias_not_reflected_in_pcm_pts(700, 700)]
#[case::codec_pts_precedes_first_packet(683, 683)]
fn original_pcm_basis_counts_the_codec_timestamp_bias_once(
    zero_packet: &'static [u8],
    #[case] output_pts: u64,
    #[case] comparison_base: u64,
) {
    let spec = test_spec(1_000);
    let expected = [0.375, -0.75];
    let codec = TimestampCodec {
        spec,
        pcm: expected.to_vec(),
        output_pts: Duration::from_millis(output_pts),
        bias: 17,
    };
    let demuxer = BoundaryFrameDemuxer {
        track: track_with_rate(1_000),
        held: zero_packet.to_vec(),
        idx: 0,
        frames: vec![BoundaryFrame {
            pts: Duration::from_millis(700),
            duration: Duration::from_millis(2),
        }],
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
    let DecoderChunkOutcome::Chunk(chunk) = decoder.next_chunk().expect("codec-timed PCM") else {
        panic!("codec-timed PCM chunk");
    };
    let origin = decoder.initial_pcm_origin().expect("original PCM origin");
    assert_eq!(chunk.meta.timestamp, Duration::from_millis(output_pts));
    assert_eq!(chunk.meta.frame_offset, output_pts);
    assert_eq!(chunk.meta.frames, 2);
    assert_eq!(chunk.samples.as_ref(), expected.as_slice());
    assert_eq!(origin.position(), Duration::from_millis(output_pts));
    assert_eq!(
        origin.comparison_base(),
        Duration::from_millis(comparison_base)
    );
    assert_eq!(decoder.timeline_gap_frames(), 17);
    assert_eq!(
        spec.frame_at(origin.comparison_base())
            .expect("basis frame")
            + decoder.timeline_gap_frames(),
        comparison_base + 17
    );
}
