use std::io::Cursor;

use kithara_stream::AudioCodec;
use kithara_test_fixtures::fixtures::tone_mp3;
#[cfg(feature = "symphonia")]
use symphonia::{
    core::{
        formats::{FormatOptions, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
    },
    default,
};

use super::*;
#[cfg(feature = "symphonia")]
use crate::symphonia::{SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer};

#[cfg(feature = "symphonia")]
pub(super) fn build_mp3_decoder(
    tone_mp3: &[u8],
) -> ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec, crate::test_pools::TestPools> {
    let cursor = Cursor::new(tone_mp3.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let format_reader = default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("BUG: MP3 probe should succeed");
    let demuxer = SymphoniaDemuxer::from_reader_with_layout(
        format_reader,
        None,
        None,
        None,
        kithara_stream::ReaderControl::new(None),
    )
    .expect("BUG: MP3 demuxer should build");
    let track_info = demuxer.track_info().clone();
    let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
        .expect("BUG: MP3 codec should open");
    ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test())
}

#[cfg(all(android_backend, not(feature = "symphonia")))]
pub(super) fn build_mp3_decoder(
    tone_mp3: &[u8],
) -> ComposedDecoder<
    crate::android::AndroidMediaExtractorDemuxer,
    crate::android::AndroidCodec,
    crate::test_pools::TestPools,
> {
    let (demuxer, format) = crate::android::AndroidMediaExtractorDemuxer::open(
        Box::new(Cursor::new(tone_mp3.to_vec())),
        AudioCodec::Mp3,
        None,
        None,
        None,
    )
    .expect("MP3 extractor");
    let codec = crate::android::AndroidCodec::open_with_format(demuxer.track_info(), &format)
        .expect("MP3 codec");
    ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test())
}

#[kithara::test]
fn prefilled_mp3_preserves_first_pcm_and_seek(tone_mp3: &'static [u8]) {
    let mut decoder = build_mp3_decoder(tone_mp3);
    decoder.prepare_next_chunk();
    let mut reference = build_mp3_decoder(tone_mp3);
    for position in [None, Some(Duration::ZERO), Some(Duration::from_millis(250))] {
        if let Some(position) = position {
            decoder.seek(position).expect("seek");
            reference.seek(position).expect("reference seek");
        }
        let actual = pcm_prefix(|| {
            decoder.prepare_next_chunk();
            decoder.prepare_next_chunk();
            decoder.next_chunk().expect("prefilled decode")
        });
        let expected = pcm_prefix(|| reference.next_chunk().expect("reference decode"));
        assert_eq!(actual, expected);
    }
}

#[kithara::test]
fn initialized_mp3_reuses_decoder_storage_after_seek(tone_mp3: &'static [u8]) {
    let mut decoder = build_mp3_decoder(tone_mp3);
    let mut reference = build_mp3_decoder(tone_mp3);
    let mut initial = Some(decoder.next_chunk().expect("initial decode"));
    for position in [None, Some(Duration::ZERO), Some(Duration::from_millis(250))] {
        if let Some(position) = position {
            decoder.seek(position).expect("seek");
            reference.seek(position).expect("reference seek");
        }
        let actual = pcm_prefix(|| {
            initial.take().unwrap_or_else(|| {
                decoder.prepare_next_chunk();
                checked_mp3_chunk(&mut decoder).expect("checked decode")
            })
        });
        let expected = pcm_prefix(|| reference.next_chunk().expect("reference decode"));
        assert_eq!(actual, expected);
    }
}

#[derive(Debug, PartialEq)]
struct Pcm {
    timestamp: Duration,
    samples: Vec<f32>,
    frame_offset: u64,
}

fn pcm_prefix(mut next: impl FnMut() -> DecoderChunkOutcome) -> Pcm {
    const FRAMES: usize = 4096;
    let mut chunks = (0..consts::ZERO_FRAME_BUDGET).filter_map(|_| match next() {
        DecoderChunkOutcome::Chunk(chunk) => Some(chunk),
        DecoderChunkOutcome::Pending(PendingReason::Retry) => None,
        _ => panic!("expected PCM"),
    });
    let first = chunks.next().expect("PCM within the call budget");
    let wanted = FRAMES * usize::from(first.meta.spec.channels);
    let mut pcm = Pcm {
        timestamp: first.meta.timestamp,
        frame_offset: first.meta.frame_offset,
        samples: first.samples.to_vec(),
    };
    while pcm.samples.len() < wanted {
        let chunk = chunks.next().expect("PCM within the call budget");
        pcm.samples.extend_from_slice(&chunk.samples);
    }
    pcm.samples.truncate(wanted);
    pcm
}

#[kithara::rtsan_forbid_blocking]
fn checked_mp3_chunk(decoder: &mut dyn Decoder) -> DecodeResult<DecoderChunkOutcome> {
    decoder.next_chunk_prepared()
}

#[kithara::test]
fn prepared_mp3_decode_preserves_pcm_and_seek(tone_mp3: &'static [u8]) {
    let mut prepared = build_mp3_decoder(tone_mp3);
    let mut regular = build_mp3_decoder(tone_mp3);
    assert!(matches!(
        prepared.next_chunk_prepared(),
        Err(DecodeError::InvalidData { .. })
    ));
    prepared.output = Some(Err(DecodeError::InvalidData {
        detail: "output preparation failed",
    }));
    assert!(prepared.has_prepared_result());
    assert!(matches!(
        prepared.next_chunk_prepared(),
        Err(DecodeError::InvalidData {
            detail: "output preparation failed"
        })
    ));
    for seek in [None, Some(Duration::from_millis(250))] {
        if let Some(position) = seek {
            prepared.seek(position).expect("prepared seek");
            regular.seek(position).expect("regular seek");
        }
        let actual = pcm_prefix(|| {
            prepared.prepare_next_chunk();
            prepared.next_chunk_prepared().expect("prepared decode")
        });
        let expected = pcm_prefix(|| regular.next_chunk().expect("regular decode"));
        assert_eq!(actual, expected);
    }
}

#[kithara::test]
fn successful_seek_discards_a_deferred_output_error(tone_mp3: &'static [u8]) {
    let mut decoder = build_mp3_decoder(tone_mp3);
    let mut reference = build_mp3_decoder(tone_mp3);
    decoder.output = Some(Err(DecodeError::InvalidData {
        detail: "old output preparation failed",
    }));
    assert!(decoder.has_prepared_result());
    let target = Duration::from_millis(250);
    decoder.seek(target).expect("new seek");
    reference.seek(target).expect("reference seek");
    assert!(!decoder.has_prepared_result());
    let actual = pcm_prefix(|| {
        decoder.prepare_next_chunk();
        decoder.next_chunk_prepared().expect("new seek PCM")
    });
    let expected = pcm_prefix(|| reference.next_chunk().expect("reference PCM"));
    assert_eq!(actual, expected);
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn composed_decoder_priming_combines_encoder_and_symphonia_mp3_algo_delay(tone_mp3: &'static [u8]) {
    let decoder = build_mp3_decoder(tone_mp3);
    // WHY: 1105 = 576 libmp3lame priming + 529 LAME algo delay.
    assert_eq!(decoder.default_priming_frames(AudioCodec::Mp3), 1105);
    assert_eq!(decoder.default_priming_frames(AudioCodec::AacLc), 1024);
    assert_eq!(decoder.default_priming_frames(AudioCodec::Opus), 312);
    assert_eq!(decoder.default_priming_frames(AudioCodec::Flac), 0);
}
