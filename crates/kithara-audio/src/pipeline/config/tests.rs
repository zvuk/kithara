#[cfg(not(target_arch = "wasm32"))]
mod native {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_decode::{GaplessMode, SilenceTrimParams};
    use kithara_file::{FileConfig, FileSrc};
    use kithara_resampler::NoResamplerBackend;
    use kithara_stream::{ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;
    use unimock::Unimock;

    use crate::{
        pipeline::config::{AudioConfig, AudioDecoderConfig, ConsumerWakeMode},
        test_pools::{TestPools, pools},
    };

    fn file_config() -> FileConfig<TestPools> {
        let pools = pools();
        FileConfig::for_src(FileSrc::Local(
            std::env::temp_dir().join("kithara-audio-config.wav"),
        ))
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .pools(pools)
        .build()
    }

    #[kithara::test]
    fn audio_config_defaults_to_realtime_deferred_consumer_wakes() {
        let config = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .build();

        assert_eq!(
            config.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
    }

    #[kithara::test]
    fn audio_config_keeps_the_native_ring_and_preload_defaults() {
        let config = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .build();

        assert_eq!(config.audio_buffer_chunks(), 10);
        assert_eq!(config.preload_chunks().get(), 3);
    }

    #[kithara::test]
    fn audio_config_observer_is_optional_and_configurable() {
        let default = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .build();
        let config = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .observer(Box::new(Unimock::new(())))
        .build();

        assert!(default.observer.is_none());
        assert!(config.observer.is_some());
    }

    #[kithara::test]
    fn audio_config_carries_the_media_info_hint() {
        let info = MediaInfo::builder()
            .container(ContainerFormat::Wav)
            .sample_rate(44100)
            .build();
        let config = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .media_info(info)
        .build();

        assert_eq!(
            config.media_info().and_then(|info| info.container),
            Some(ContainerFormat::Wav)
        );
    }

    #[kithara::test]
    #[case::codec_priming(GaplessMode::CodecPriming)]
    #[case::silence_trim(GaplessMode::SilenceTrim(SilenceTrimParams {
        threshold_db: 50.0,
        min_trim_frames: 128,
        scan_window_frames: 2_048,
        trim_trailing: true,
    }))]
    fn audio_config_carries_the_decoder_gapless_mode(#[case] mode: GaplessMode) {
        let config = AudioConfig::<kithara_file::File<TestPools>, NoResamplerBackend>::for_stream(
            file_config(),
        )
        .decoder(AudioDecoderConfig::builder().gapless_mode(mode).build())
        .build();

        assert_eq!(config.decoder().gapless_mode(), mode);
    }
}
