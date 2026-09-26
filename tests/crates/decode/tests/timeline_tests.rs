#[cfg(not(target_arch = "wasm32"))]
mod hls_timeline {
    use kithara::{
        assets::{AssetStore, StorageBackend},
        decode::{DecoderConfig, DecoderFactory},
        hls::{AbrMode, Hls, HlsConfig},
        platform::{CancelToken, sync::Arc, time::Duration, tokio},
        resampler::NoResamplerBackend,
        stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
    };
    use kithara_integration_tests::{
        CreatedHls, HlsFixtureBuilder, TestServerHelper,
        bufpool_ext::{TestPools, pools},
    };
    use kithara_test_fixtures::assets::sized_wav_timeline_saw_2mb;
    use kithara_test_utils::TestTempDir;

    use crate::common::test_defaults::SawWav;

    #[kithara::fixture]
    async fn timeline_server() -> CreatedHls {
        const SEGMENT_COUNT: usize = 10;
        let segment_duration = SawWav::DEFAULT.segment_size as f64
            / (f64::from(SawWav::DEFAULT.sample_rate) * f64::from(SawWav::DEFAULT.channels) * 2.0);

        let wav = tokio::task::spawn_blocking(|| sized_wav_timeline_saw_2mb().bytes().to_vec())
            .await
            .expect("read prepared timeline WAV");
        TestServerHelper::new()
            .await
            .create_hls(
                HlsFixtureBuilder::new()
                    .segments_per_variant(SEGMENT_COUNT)
                    .segment_size(SawWav::DEFAULT.segment_size)
                    .segment_duration_secs(segment_duration)
                    .custom_data(Arc::new(wav)),
            )
            .await
            .expect("create HLS fixture")
    }

    #[kithara::test(
        tokio,
        timeout(Duration::from_secs(10)),
        hang_timeout_secs(1),
        tracing("kithara_decode=debug,kithara_hls=debug,kithara_stream=debug")
    )]
    async fn test_hls_timeline_segment_tracking(#[future(awt)] timeline_server: CreatedHls) {
        let server = timeline_server;
        let url = server.master_url();
        let temp_dir = TestTempDir::new();
        let cancel = CancelToken::never();
        let pools = pools();

        let hls_config = HlsConfig::for_url(url)
            .store(
                AssetStore::builder(pools.clone())
                    .backend(StorageBackend::Disk {
                        root: temp_dir.path().to_path_buf(),
                    })
                    .build(),
            )
            .pools(pools.clone())
            .cancel(cancel)
            .initial_abr_mode(AbrMode::manual(0))
            .build();

        let stream = Stream::<Hls<TestPools>>::new(hls_config).await.unwrap();

        let wav_info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Pcm))
            .maybe_container(Some(ContainerFormat::Wav))
            .build();
        let decoder_config = DecoderConfig::<NoResamplerBackend, TestPools>::builder()
            .pools(pools)
            .hint("wav")
            .maybe_byte_map(stream.byte_map())
            .build();

        let result = tokio::task::spawn_blocking(move || {
            let mut decoder =
                DecoderFactory::create_from_media_info(stream, &wav_info, decoder_config).unwrap();

            let mut prev_frame_end = 0u64;
            let mut chunk_count = 0u64;
            let mut max_segment_index = 0u32;

            while let Ok(kithara::decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk()
            {
                let meta = chunk.meta;

                assert_eq!(meta.spec.sample_rate.get(), SawWav::DEFAULT.sample_rate);
                assert_eq!(meta.spec.channels, SawWav::DEFAULT.channels);

                assert_eq!(
                    meta.frame_offset, prev_frame_end,
                    "frame_offset gap at chunk {chunk_count}: expected {prev_frame_end}, got {}",
                    meta.frame_offset
                );

                let expected_ts = Duration::from_secs_f64(
                    meta.frame_offset as f64 / f64::from(meta.spec.sample_rate.get()),
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
