//! E2E tests verifying `PcmMeta` timeline correctness.

use std::{io::Cursor, time::Duration};

use kithara::decode::{DecoderConfig, DecoderFactory};
use kithara_integration_tests::audio_fixture::EmbeddedAudio;
#[kithara::test]
fn test_progressive_file_timeline_monotonic() {
    let audio = EmbeddedAudio::get();
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), DecoderConfig::default()).unwrap();

    let mut prev_frame_end = 0u64;
    let mut chunk_count = 0u64;

    while let Ok(Some(chunk)) = decoder.next_chunk() {
        let meta = chunk.meta;

        // Ensure the spec is correct
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

#[kithara::test]
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

    // frame_offset should be approximately at the seek position
    let diff = (chunk.meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
    assert!(
        diff < 2048,
        "frame_offset after seek: {} expected ~{}",
        chunk.meta.frame_offset,
        expected_frame
    );
}

// HLS timeline tests — native-only (uses custom_data which is not supported on WASM fixture server)

#[cfg(not(target_arch = "wasm32"))]
mod hls_timeline {
    use std::{sync::Arc, time::Duration};

    use kithara::{
        assets::StoreOptions,
        decode::{DecoderConfig, DecoderFactory},
        hls::{AbrMode, AbrOptions, Hls, HlsConfig},
        stream::{AudioCodec, ContainerFormat, MediaInfo, Stream, StreamType},
    };
    use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
    use kithara_test_utils::{TestTempDir, create_wav_exact_bytes, signal_pcm::signal};
    use tokio_util::sync::CancellationToken;

    use crate::common::test_defaults::SawWav;

    const D: SawWav = SawWav::DEFAULT;
    const SEGMENT_COUNT: usize = 10;
    const TOTAL_BYTES: usize = SEGMENT_COUNT * D.segment_size; // 2 MB

    /// Verify that HLS stream produces correct `PcmMeta` with segment tracking.
    #[kithara::test(
        tokio,
        timeout(Duration::from_secs(10)),
        env(KITHARA_HANG_TIMEOUT_SECS = "1"),
        tracing("kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
    )]
    async fn test_hls_timeline_segment_tracking() {
        // Generate WAV data served as HLS segments
        let wav_data =
            create_wav_exact_bytes(signal::Sawtooth, D.sample_rate, D.channels, TOTAL_BYTES);

        let segment_duration =
            D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

        let server = HlsTestServer::new(HlsTestServerConfig {
            segments_per_variant: SEGMENT_COUNT,
            segment_size: D.segment_size,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::new(wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8").unwrap();
        let temp_dir = TestTempDir::new();
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::new(url)
            .with_store(StoreOptions::new(temp_dir.path()))
            .with_cancel(cancel)
            .with_abr_options(AbrOptions {
                mode: AbrMode::Manual(0),
                ..AbrOptions::default()
            });

        // Create an HLS stream and build StreamContext before moving stream to decoder
        let stream = Stream::<Hls>::new(hls_config).await.unwrap();
        let stream_ctx = Hls::build_stream_context(stream.source(), stream.timeline());

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

                assert_eq!(meta.spec.sample_rate, D.sample_rate);
                assert_eq!(meta.spec.channels, D.channels);

                assert_eq!(
                    meta.frame_offset, prev_frame_end,
                    "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
                    meta.frame_offset
                );

                let expected_ts = Duration::from_secs_f64(
                    meta.frame_offset as f64 / meta.spec.sample_rate as f64,
                );
                let diff = if meta.timestamp > expected_ts {
                    meta.timestamp - expected_ts
                } else {
                    expected_ts - meta.timestamp
                };
                assert!(
                    diff < Duration::from_millis(1),
                    "timestamp drift: {diff:?} at chunk {chunk_count}"
                );

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

                assert_eq!(
                    meta.variant_index,
                    Some(0),
                    "variant_index should be 0 at chunk {chunk_count}"
                );

                assert_eq!(meta.epoch, 0, "epoch should stay 0 at chunk {chunk_count}");

                prev_frame_end = meta.frame_offset + chunk.frames() as u64;
                chunk_count += 1;
            }

            assert!(chunk_count > 0, "should have decoded some chunks");
            assert!(
                max_segment_index > 0,
                "should have crossed segment boundaries (max_segment_index={max_segment_index})"
            );

            (chunk_count, max_segment_index)
        })
        .await;

        let (chunks, max_seg) = result.expect("spawn_blocking failed");
        tracing::info!(chunks, max_segment = max_seg, "HLS timeline test passed");
    }
}
