use std::{io::Cursor, time::Duration};

use kithara::decode::{DecoderConfig, DecoderFactory, PcmChunk};
use kithara_integration_tests::audio_fixture::EmbeddedAudio;
#[kithara::test]
fn test_progressive_file_timeline_monotonic() {
    let audio = EmbeddedAudio::get();
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), &DecoderConfig::default()).unwrap();

    let mut prev_frame_end = 0u64;
    let mut chunk_count = 0u64;

    while let Ok(kithara_decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk() {
        let meta = chunk.meta;

        assert_eq!(meta.spec.sample_rate, 44100);
        assert_eq!(meta.spec.channels, 2);

        assert_eq!(
            meta.frame_offset, prev_frame_end,
            "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
            meta.frame_offset
        );

        let expected_ts =
            Duration::from_secs_f64(meta.frame_offset as f64 / f64::from(meta.spec.sample_rate));
        let diff = meta.timestamp.abs_diff(expected_ts);
        assert!(
            diff < Duration::from_micros(100),
            "timestamp drift: {diff:?}"
        );

        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);

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
        DecoderFactory::create_with_probe(reader, Some("wav"), &DecoderConfig::default()).unwrap();

    for _ in 0..3 {
        let _ = decoder.next_chunk();
    }

    decoder.seek(Duration::from_millis(500)).unwrap();

    let chunk = PcmChunk::try_from(decoder.next_chunk().unwrap()).unwrap();
    let expected_frame = (0.5 * 44100.0) as u64;

    let diff = (chunk.meta.frame_offset as i64 - expected_frame as i64).unsigned_abs();
    assert!(
        diff < 2048,
        "frame_offset after seek: {} expected ~{}",
        chunk.meta.frame_offset,
        expected_frame
    );
}

#[cfg(not(target_arch = "wasm32"))]
mod hls_timeline {
    use std::{sync::Arc, time::Duration};

    use kithara::{
        assets::StoreOptions,
        decode::{DecoderConfig, DecoderFactory},
        hls::{AbrMode, Hls, HlsConfig},
        stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
    };
    use kithara_integration_tests::{
        TestTempDir, create_wav_exact_bytes,
        hls_server::{HlsTestServer, HlsTestServerConfig},
        signal_pcm::signal,
    };
    use tokio_util::sync::CancellationToken;

    use crate::common::test_defaults::SawWav;

    #[kithara::test(
        tokio,
        timeout(Duration::from_secs(10)),
        env(KITHARA_HANG_TIMEOUT_SECS = "1"),
        tracing("kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
    )]
    async fn test_hls_timeline_segment_tracking() {
        const SEGMENT_COUNT: usize = 10;
        const TOTAL_BYTES: usize = SEGMENT_COUNT * SawWav::DEFAULT.segment_size;

        let wav_data = create_wav_exact_bytes(
            signal::Sawtooth,
            SawWav::DEFAULT.sample_rate,
            SawWav::DEFAULT.channels,
            TOTAL_BYTES,
        );

        let segment_duration = SawWav::DEFAULT.segment_size as f64
            / (f64::from(SawWav::DEFAULT.sample_rate) * f64::from(SawWav::DEFAULT.channels) * 2.0);

        let server = HlsTestServer::new(HlsTestServerConfig {
            segments_per_variant: SEGMENT_COUNT,
            segment_size: SawWav::DEFAULT.segment_size,
            segment_duration_secs: segment_duration,
            custom_data: Some(Arc::new(wav_data)),
            ..Default::default()
        })
        .await;

        let url = server.url("/master.m3u8");
        let temp_dir = TestTempDir::new();
        let cancel = CancellationToken::new();

        let hls_config = HlsConfig::for_url(url)
            .store(StoreOptions::new(temp_dir.path()))
            .cancel(cancel)
            .initial_abr_mode(AbrMode::Manual(0))
            .build();

        let stream = Stream::<Hls>::new(hls_config).await.unwrap();

        let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
        let decoder_config = DecoderConfig::builder()
            .hint("wav".to_string())
            .maybe_segment_layout(stream.as_segment_layout())
            .build();

        let result = tokio::task::spawn_blocking(move || {
            let mut decoder =
                DecoderFactory::create_from_media_info(stream, &wav_info, &decoder_config).unwrap();

            let mut prev_frame_end = 0u64;
            let mut chunk_count = 0u64;
            let mut max_segment_index = 0u32;

            while let Ok(kithara_decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk() {
                let meta = chunk.meta;

                assert_eq!(meta.spec.sample_rate, SawWav::DEFAULT.sample_rate);
                assert_eq!(meta.spec.channels, SawWav::DEFAULT.channels);

                assert_eq!(
                    meta.frame_offset, prev_frame_end,
                    "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
                    meta.frame_offset
                );

                let expected_ts = Duration::from_secs_f64(
                    meta.frame_offset as f64 / f64::from(meta.spec.sample_rate),
                );
                let diff = meta.timestamp.abs_diff(expected_ts);
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

                if let Some(seg) = meta.segment_index
                    && seg > max_segment_index
                {
                    max_segment_index = seg;
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
