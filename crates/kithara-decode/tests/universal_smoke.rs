//! Smoke test for the unified `Demuxer + FrameCodec → UniversalDecoder<D, C>`
//! path on a real MP3 fixture.
//!
//! Validates that:
//! - `SymphoniaDemuxer::from_reader` builds from a probed `FormatReader`
//!   and exposes a sane `TrackInfo`.
//! - `SymphoniaCodec::open(track_info)` constructs a Symphonia codec.
//! - `UniversalDecoder<SymphoniaDemuxer, SymphoniaCodec>` produces
//!   non-empty `PcmChunk` values via `Decoder::next_chunk`.

#![cfg(feature = "symphonia")]

use std::io::Cursor;

use kithara_bufpool::pcm_pool;
use kithara_decode::{
    Decoder, DecoderChunkOutcome, Demuxer, FrameCodec, SymphoniaCodec, SymphoniaDemuxer,
    UniversalDecoder,
};
use kithara_stream::AudioCodec;
use kithara_test_utils::kithara;
use symphonia::core::{
    formats::{FormatOptions, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
};

const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/test.mp3"
));

fn build_mp3_demuxer() -> SymphoniaDemuxer {
    let cursor = Cursor::new(TEST_MP3_BYTES.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let format_reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("MP3 probe should succeed");
    SymphoniaDemuxer::from_reader(format_reader, None).expect("MP3 demuxer should build")
}

#[kithara::test]
fn mp3_track_info_carries_codec_and_rate() {
    let demuxer = build_mp3_demuxer();
    let info = demuxer.track_info();
    assert_eq!(info.codec, AudioCodec::Mp3);
    assert!(info.sample_rate > 0, "sample rate must be populated");
    assert!(info.channels > 0, "channels must be populated");
}

#[kithara::test]
fn mp3_universal_decoder_emits_non_empty_chunks() {
    let demuxer = build_mp3_demuxer();
    let track_info = demuxer.track_info().clone();
    let codec = SymphoniaCodec::open(&track_info).expect("MP3 codec should open");
    let mut decoder = UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None);

    // Pull a handful of chunks; require that at least one carries
    // non-empty PCM with a sane spec. Some early MP3 packets may decode
    // to zero frames (warm-up); the loop accepts that and pulls more.
    let mut got_chunk = false;
    for _ in 0..16 {
        match decoder.next_chunk().expect("next_chunk should not error") {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(chunk.frames() > 0, "Chunk frames must be > 0");
                assert!(chunk.spec().sample_rate > 0);
                assert!(chunk.spec().channels > 0);
                got_chunk = true;
                break;
            }
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => panic!("MP3 fixture must not EOF in 16 packets"),
        }
    }
    assert!(got_chunk, "UniversalDecoder must emit at least one chunk");
}

#[kithara::test]
fn mp3_universal_decoder_seeks_back_to_start_after_pulling_chunks() {
    use std::time::Duration;

    use kithara_decode::DecoderSeekOutcome;

    let demuxer = build_mp3_demuxer();
    let track_info = demuxer.track_info().clone();
    let codec = SymphoniaCodec::open(&track_info).expect("MP3 codec should open");
    let mut decoder = UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None);

    for _ in 0..4 {
        let _ = decoder
            .next_chunk()
            .expect("priming chunks should not error");
    }

    let outcome = decoder
        .seek(Duration::ZERO)
        .expect("seek to start must not error");
    match outcome {
        DecoderSeekOutcome::Landed { landed_at, .. } => {
            assert!(
                landed_at < Duration::from_millis(50),
                "seek to ZERO should land near 0, got {landed_at:?}"
            );
        }
        DecoderSeekOutcome::PastEof { .. } => panic!("seek(0) on a real MP3 must not be PastEof"),
    }
}
