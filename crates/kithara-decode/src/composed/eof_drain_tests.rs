use std::{num::NonZeroU32, sync::atomic::Ordering};

use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::AudioCodec;
use kithara_test_fixtures::{mock_fixtures::one_packet, unit_fixtures::trim_silence};
use kithara_test_utils::kithara;

use super::{
    test_eof_drain_codec::{EofDrainCodec, QueueCodec},
    *,
};
use crate::{
    demuxer::{DemuxOutcome, Frame, TrackInfo},
    traits::Decoder,
};

struct OneFrameDemuxer {
    track: TrackInfo,
    held: Vec<u8>,
    emitted: bool,
}

impl Demuxer for OneFrameDemuxer {
    fn duration(&self) -> Option<crate::DecoderDuration> {
        None
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if self.emitted {
            return Ok(DemuxOutcome::Eof);
        }
        self.emitted = true;
        Ok(DemuxOutcome::Frame(Frame {
            pts: Duration::ZERO,
            duration: Duration::from_millis(20),
            data: &self.held,
            packet_desc: &[],
        }))
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track
    }

    reset_demuxer_seek!(emitted = false);
}

fn empty_track() -> TrackInfo {
    TrackInfo {
        codec: AudioCodec::AacLc,
        sample_rate: 44_100,
        channels: 2,
        extra_data: Vec::new(),
        duration: None,
        gapless: None,
    }
}

#[kithara::test]
fn rate_mismatch_default_codec_drains_tail_before_eof(
    trim_silence: Vec<f32>,
    one_packet: &'static [u8],
) {
    const FRAMES: u32 = 960;
    const TAIL_FRAMES: u32 = 17;
    const SAMPLE_RATE: u32 = 48_000;

    let codec = EofDrainCodec::new(
        trim_silence.clone(),
        AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
        FRAMES,
        TAIL_FRAMES,
    );
    let empty_calls = Arc::clone(&codec.empty_decode_calls);
    let demuxer = OneFrameDemuxer {
        track: empty_track(),
        held: one_packet.to_vec(),
        emitted: false,
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

    let first = decoder.next_chunk().expect("BUG: first chunk");
    let DecoderChunkOutcome::Chunk(first_chunk) = first else {
        panic!("expected first Chunk, got {first:?}");
    };
    let drained = decoder.next_chunk().expect("BUG: EOF drain chunk");
    let DecoderChunkOutcome::Chunk(drained_chunk) = drained else {
        panic!("expected drained Chunk, got {drained:?}");
    };
    let eof = decoder.next_chunk().expect("BUG: EOF after drain");

    assert!(matches!(eof, DecoderChunkOutcome::Eof));
    assert_eq!(first_chunk.meta.frames, FRAMES);
    assert_eq!(drained_chunk.meta.frames, TAIL_FRAMES);
    assert_eq!(drained_chunk.meta.frame_offset, u64::from(FRAMES));
    assert_eq!(
        drained_chunk.meta.timestamp,
        AudioSpec::new(
            1,
            NonZeroU32::new(SAMPLE_RATE).expect("test sample rate is non-zero"),
        )
        .duration_for(u64::from(FRAMES))
        .expect("test duration is representable")
    );
    assert_eq!(empty_calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        decoder
            .next_chunk_prepared()
            .expect("EOF retains prepared output"),
        DecoderChunkOutcome::Eof
    ));
}

#[kithara::test]
fn equal_rate_queue_codec_emits_tail_before_eof(trim_silence: Vec<f32>, one_packet: &'static [u8]) {
    const TAIL_FRAMES: u32 = 17;
    const SAMPLE_RATE: u32 = 44_100;

    let codec = QueueCodec::new(
        trim_silence.clone(),
        AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
        TAIL_FRAMES,
    );
    let empty_calls = Arc::clone(&codec.empty_decode_calls);
    let demuxer = OneFrameDemuxer {
        track: empty_track(),
        held: one_packet.to_vec(),
        emitted: false,
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

    let drained = decoder.next_chunk().expect("BUG: EOF drain chunk");
    let DecoderChunkOutcome::Chunk(drained_chunk) = drained else {
        panic!("expected drained Chunk, got {drained:?}");
    };
    let eof = decoder.next_chunk().expect("BUG: EOF after drain");

    assert_eq!(drained_chunk.meta.frames, TAIL_FRAMES);
    assert!(matches!(eof, DecoderChunkOutcome::Eof));
    assert_eq!(empty_calls.load(Ordering::SeqCst), 2);
}

#[kithara::test]
fn equal_rate_default_codec_skips_eof_drain(trim_silence: Vec<f32>, one_packet: &'static [u8]) {
    const FRAMES: u32 = 960;
    const TAIL_FRAMES: u32 = 17;
    const SAMPLE_RATE: u32 = 44_100;

    let codec = EofDrainCodec::new(
        trim_silence.clone(),
        AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("test rate")),
        FRAMES,
        TAIL_FRAMES,
    );
    let empty_calls = Arc::clone(&codec.empty_decode_calls);
    let demuxer = OneFrameDemuxer {
        track: empty_track(),
        held: one_packet.to_vec(),
        emitted: false,
    };
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

    let first = decoder.next_chunk().expect("BUG: first chunk");
    let eof = decoder.next_chunk().expect("BUG: EOF without drain");

    assert!(matches!(first, DecoderChunkOutcome::Chunk(_)));
    assert!(matches!(eof, DecoderChunkOutcome::Eof));
    assert_eq!(empty_calls.load(Ordering::SeqCst), 0);
}
