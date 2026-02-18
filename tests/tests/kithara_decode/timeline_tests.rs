//! E2E tests verifying `PcmMeta` timeline correctness.

use std::{io::Cursor, sync::Arc, time::Duration};

use kithara::{
    assets::StoreOptions,
    decode::{DecoderConfig, DecoderFactory},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType},
};
use kithara_test_utils::wav::create_saw_wav;
use rstest::rstest;
use tokio_util::sync::CancellationToken;

use super::fixture::EmbeddedAudio;
use crate::kithara_hls::fixture::{HlsTestServer, HlsTestServerConfig};

#[test]
fn test_progressive_file_timeline_monotonic() {
    let audio = EmbeddedAudio::get();
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), DecoderConfig::default()).unwrap();

    let mut prev_frame_end = 0u64;
    let mut chunk_count = 0u64;

    while let Ok(Some(chunk)) = decoder.next_chunk() {
        let meta = chunk.meta;

        // Spec correct
        assert_eq!(meta.spec.sample_rate, 44100);
        assert_eq!(meta.spec.channels, 2);

        // Frame offset continuous (no gaps)
        assert_eq!(
            meta.frame_offset, prev_frame_end,
            "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
            meta.frame_offset
        );

        // Timestamp matches frame_offset / sample_rate
        let expected_ts =
            Duration::from_secs_f64(meta.frame_offset as f64 / meta.spec.sample_rate as f64);
        let diff = if meta.timestamp > expected_ts {
            meta.timestamp - expected_ts
        } else {
            expected_ts - meta.timestamp
        };
        assert!(
            diff < Duration::from_micros(100),
            "timestamp drift: {diff:?}"
        );

        // Progressive file: no segment/variant
        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);

        // Epoch stays 0
        assert_eq!(meta.epoch, 0);

        prev_frame_end = meta.frame_offset + chunk.frames() as u64;
        chunk_count += 1;
    }

    assert!(chunk_count > 0, "should have decoded some chunks");
}

#[test]
fn test_progressive_file_seek_resets_frame_offset() {
    let audio = EmbeddedAudio::get();
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), DecoderConfig::default()).unwrap();

    // Read a few chunks
    for _ in 0..3 {
        let _ = decoder.next_chunk();
    }

    // Seek to 0.5s
    decoder.seek(Duration::from_millis(500)).unwrap();

    let chunk = decoder.next_chunk().unwrap().unwrap();
    let expected_frame = (0.5 * 44100.0) as u64;

    // frame_offset should be approximately at seek position
    let diff = (chunk.meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
    assert!(
        diff < 2048,
        "frame_offset after seek: {} expected ~{}",
        chunk.meta.frame_offset,
        expected_frame
    );
}

// HLS timeline tests

const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 200_000;
const SEGMENT_COUNT: usize = 10;
const TOTAL_BYTES: usize = SEGMENT_COUNT * SEGMENT_SIZE; // 2 MB

/// Verify that HLS stream produces correct `PcmMeta` with segment tracking.
///
/// Checks:
/// - `segment_index` is `Some` and increments as we cross segment boundaries
/// - `variant_index` is `Some` and consistent
/// - `frame_offset` monotonically increases
/// - `epoch` stays 0 (no ABR switch)
#[rstest]
#[timeout(Duration::from_secs(30))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_hls_timeline_segment_tracking() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "kithara_decode=debug,kithara_hls=debug,kithara_stream=debug".to_string()
        }))
        .try_init();

    // Generate WAV data served as HLS segments
    let wav_data = create_saw_wav(TOTAL_BYTES);
    let segment_duration = SEGMENT_SIZE as f64 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data: Some(Arc::new(wav_data)),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").unwrap();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let cancel = CancellationToken::new();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_abr(AbrOptions {
            mode: AbrMode::Manual(0),
            ..AbrOptions::default()
        });

    // Create HLS stream and build StreamContext before moving stream to decoder
    let stream = Stream::<Hls>::new(hls_config).await.unwrap();
    let stream_ctx = Hls::build_stream_context(stream.source(), stream.position_handle());

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let decoder_config = DecoderConfig {
        stream_ctx: Some(stream_ctx),
        hint: Some("wav".to_string()),
        ..Default::default()
    };

    // Decode in blocking thread (Stream<Hls> is sync Read+Seek)
    let result = tokio::task::spawn_blocking(move || {
        let mut decoder =
            DecoderFactory::create_from_media_info(stream, &wav_info, decoder_config).unwrap();

        let mut prev_frame_end = 0u64;
        let mut chunk_count = 0u64;
        let mut max_segment_index = 0u32;

        while let Ok(Some(chunk)) = decoder.next_chunk() {
            let meta = chunk.meta;

            // Spec correct
            assert_eq!(meta.spec.sample_rate, SAMPLE_RATE);
            assert_eq!(meta.spec.channels, CHANNELS);

            // Frame offset continuous (no gaps)
            assert_eq!(
                meta.frame_offset, prev_frame_end,
                "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
                meta.frame_offset
            );

            // Timestamp matches frame_offset / sample_rate
            let expected_ts =
                Duration::from_secs_f64(meta.frame_offset as f64 / meta.spec.sample_rate as f64);
            let diff = if meta.timestamp > expected_ts {
                meta.timestamp - expected_ts
            } else {
                expected_ts - meta.timestamp
            };
            assert!(
                diff < Duration::from_millis(1),
                "timestamp drift: {diff:?} at chunk {chunk_count}"
            );

            // HLS: segment_index and variant_index should be present
            assert!(
                meta.segment_index.is_some(),
                "segment_index should be Some for HLS at chunk {chunk_count}"
            );
            assert!(
                meta.variant_index.is_some(),
                "variant_index should be Some for HLS at chunk {chunk_count}"
            );

            if let Some(seg) = meta.segment_index {
                if seg > max_segment_index {
                    max_segment_index = seg;
                }
            }

            // Variant stays at 0 (Manual mode, single variant)
            assert_eq!(
                meta.variant_index,
                Some(0),
                "variant_index should be 0 at chunk {chunk_count}"
            );

            // Epoch stays 0
            assert_eq!(meta.epoch, 0, "epoch should stay 0 at chunk {chunk_count}");

            prev_frame_end = meta.frame_offset + chunk.frames() as u64;
            chunk_count += 1;
        }

        assert!(chunk_count > 0, "should have decoded some chunks");

        // Verify we saw multiple segments
        assert!(
            max_segment_index > 0,
            "should have crossed segment boundaries (max_segment_index={max_segment_index})"
        );

        (chunk_count, max_segment_index)
    })
    .await;

    match result {
        Ok((chunks, max_seg)) => {
            tracing::info!(chunks, max_segment = max_seg, "HLS timeline test passed");
        }
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
