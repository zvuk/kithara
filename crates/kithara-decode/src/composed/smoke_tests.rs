#[cfg(feature = "symphonia")]
use std::io::Cursor;

use kithara_test_fixtures::fixtures::tone_mp3;

use super::{default_priming_tests::build_mp3_decoder, *};
#[cfg(feature = "symphonia")]
use crate::symphonia::{FileOpen, SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer};

#[kithara::test]
fn mp3_track_info_carries_codec_and_rate(tone_mp3: &'static [u8]) {
    let decoder = build_mp3_decoder(tone_mp3);
    let info = decoder.demuxer.track_info();
    assert_eq!(info.codec, AudioCodec::Mp3);
    assert!(info.sample_rate > 0, "sample rate must be populated");
    assert!(info.channels > 0, "channels must be populated");
}

#[kithara::test]
fn mp3_universal_decoder_emits_non_empty_chunks(tone_mp3: &'static [u8]) {
    let mut decoder = build_mp3_decoder(tone_mp3);

    let mut got_chunk = false;
    for _ in 0..16 {
        match decoder
            .next_chunk()
            .expect("BUG: next_chunk should not error")
        {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(chunk.frames() > 0, "Chunk frames must be > 0");
                assert!(chunk.spec().sample_rate.get() > 0);
                assert!(chunk.spec().channels > 0);
                got_chunk = true;
                break;
            }
            DecoderChunkOutcome::Pending(_) => continue,
            DecoderChunkOutcome::Eof => panic!("MP3 fixture must not EOF in 16 packets"),
        }
    }
    assert!(got_chunk, "ComposedDecoder must emit at least one chunk");
}

#[kithara::test]
fn mp3_universal_decoder_seeks_back_to_start_after_pulling_chunks(tone_mp3: &'static [u8]) {
    let mut decoder = build_mp3_decoder(tone_mp3);

    for _ in 0..4 {
        let _ = decoder
            .next_chunk()
            .expect("BUG: priming chunks should not error");
    }

    let outcome = decoder
        .seek(Duration::ZERO)
        .expect("BUG: seek to start must not error");
    match outcome {
        DecoderSeekOutcome::Landed { landed_at, .. } => {
            assert!(
                landed_at < Duration::from_millis(50),
                "seek to ZERO should land near 0, got {landed_at:?}"
            );
        }
        DecoderSeekOutcome::Pending(reason) => {
            panic!("in-memory MP3 seek must resolve synchronously: {reason:?}")
        }
        DecoderSeekOutcome::PastEof { .. } => {
            panic!("seek(0) on a real MP3 must not be PastEof")
        }
    }
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn symphonia_mp3_demuxer_emits_notneeded_preroll_after_seek(tone_mp3: &'static [u8]) {
    let (demuxer, _byte_len_handle) = SymphoniaDemuxer::open_file(
        Cursor::new(tone_mp3),
        FileOpen {
            control: kithara_stream::ReaderControl::new(None),
            hint: Some("mp3".into()),
            container: None,
            byte_len_handle: None,
            byte_map: None,
        },
    )
    .expect("BUG: open_file must succeed");
    let track_info = demuxer.track_info().clone();
    let codec = SymphoniaCodec::open_with_config(&track_info, &SymphoniaConfig::default())
        .expect("BUG: MP3 codec should open");
    let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());

    let outcome = decoder.seek(Duration::from_secs(1)).expect("BUG: seek");
    let DecoderSeekOutcome::Landed {
        landed_byte,
        preroll,
        ..
    } = outcome
    else {
        panic!("expected Landed, got {outcome:?}");
    };
    assert!(
        landed_byte.is_some(),
        "BUG: symphonia MP3 must expose landed_byte"
    );
    assert_eq!(
        preroll,
        kithara_stream::PrerollHint::NotNeeded,
        "Symphonia handles MDCT priming internally; preroll must be NotNeeded"
    );
}
