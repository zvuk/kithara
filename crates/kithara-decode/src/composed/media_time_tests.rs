use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;
use kithara_stream::PrerollHint;
use kithara_test_fixtures::mock_fixtures::zero_packet;
use kithara_test_utils::kithara;

use super::{
    ComposedDecoder, DecoderRuntime,
    seek_trim_tests::{BoundaryFrame, BoundaryFrameDemuxer, test_spec, track_with_rate},
    test_counting_codec::CountingCodec,
};
use crate::{
    CodecPriming, DecodeResult, Decoder, DecoderChunkOutcome,
    codec::FrameCodec,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, TrackInfo},
};

struct NativeClock {
    inner: BoundaryFrameDemuxer,
    media_origin: Option<Duration>,
}

impl Demuxer for NativeClock {
    fn duration(&self) -> Option<crate::DecoderDuration> {
        Some(crate::DecoderDuration::MediaEnd(Duration::from_secs(2)))
    }
    fn media_origin(&self) -> Option<Duration> {
        self.media_origin
    }

    delegate::delegate! {
        to self.inner {
            fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>>;
            fn track_info(&self) -> &TrackInfo;
        }
    }

    fn seek(&mut self, target: Duration, _priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.inner.idx = 0;
        Ok(DemuxSeekOutcome::Landed {
            landed_at: target,
            landed_byte: None,
            preroll: PrerollHint::NotNeeded,
        })
    }
}

struct RebasedCodec {
    inner: CountingCodec,
    rebase: Option<Duration>,
}

impl FrameCodec for RebasedCodec {
    fn media_time_rebase(&self) -> Option<Duration> {
        self.rebase
    }

    delegate::delegate! {
        to self.inner {
                fn decode_frame(
                    &mut self,
                    frame: crate::demuxer::Frame<'_>,
            metadata: crate::demuxer::PacketMetadata,
                    out: &mut SampleBuffer,
                ) -> DecodeResult<crate::codec::DecodedPacket>;
                fn flush(&mut self) -> DecodeResult<()>;
                fn spec(&self) -> AudioSpec;
        }
    }
}

#[kithara::test]
#[case::native_identity(Some(0), Some(0), Some(0))]
#[case::native_mpeg_shift(Some(1_105), Some(0), Some(1_105))]
#[case::codec_rebases_removed_head(Some(1_105), Some(529), Some(576))]
#[case::unknown_native_translation(None, Some(0), None)]
#[case::unknown_codec_translation(Some(1_105), None, None)]
#[case::unrepresentable_media_zero(Some(0), Some(529), None)]
fn media_zero_uses_native_translation_and_codec_rebase_without_observing_pcm(
    zero_packet: &'static [u8],
    #[case] native: Option<u64>,
    #[case] removed: Option<u64>,
    #[case] expected: Option<u64>,
) {
    let spec = test_spec(1_000);
    let samples = [0.25, -0.5];
    let demuxer = NativeClock {
        inner: BoundaryFrameDemuxer {
            track: track_with_rate(1_000),
            frames: vec![BoundaryFrame {
                pts: Duration::from_millis(1_200),
                duration: Duration::from_millis(2),
            }],
            held: zero_packet.to_vec(),
            idx: 0,
        },
        media_origin: native.map(Duration::from_millis),
    };
    let codec = RebasedCodec {
        inner: CountingCodec::new(samples.to_vec(), spec, 2),
        rebase: removed.map(Duration::from_millis),
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
    let expected = expected.map(Duration::from_millis);
    assert_eq!(decoder.media_origin(), expected);
    assert_eq!(
        decoder.duration(),
        Some(crate::DecoderDuration::MediaEnd(Duration::from_secs(2)))
    );
    assert_eq!(decoder.initial_pcm_origin(), None);
    assert_eq!(
        decoder.duration(),
        Some(crate::DecoderDuration::MediaEnd(Duration::from_secs(2)))
    );

    decoder
        .seek(Duration::from_millis(1_200))
        .expect("seek before original PCM");
    assert_eq!(decoder.media_origin(), expected);
    assert_eq!(decoder.initial_pcm_origin(), None);
    let DecoderChunkOutcome::Chunk(first) = decoder.next_chunk().expect("sought PCM") else {
        panic!("sought PCM chunk");
    };
    assert_eq!(first.meta.media_origin, expected);
    assert_eq!(first.meta.timestamp, Duration::from_millis(1_200));
    assert_eq!(first.meta.frame_offset, 1_200);
    assert_eq!(first.samples.as_ref(), samples.as_slice());
    assert_eq!(decoder.initial_pcm_origin(), None);

    decoder
        .seek(Duration::ZERO)
        .expect("return to original coordinate");
    let DecoderChunkOutcome::Chunk(replayed) = decoder.next_chunk().expect("replayed PCM") else {
        panic!("replayed PCM chunk");
    };
    assert_eq!(decoder.media_origin(), expected);
    assert_eq!(replayed.meta.media_origin, expected);
    assert_eq!(replayed.meta.timestamp, first.meta.timestamp);
    assert_eq!(replayed.samples.as_ref(), first.samples.as_ref());
}
