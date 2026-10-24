#[cfg(not(target_arch = "wasm32"))]
mod native {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_file::{FileConfig, FileSrc};
    use kithara_resampler::NoResamplerBackend;
    use kithara_stream::{ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;
    use unimock::Unimock;

    use crate::{
        pipeline::config::AudioConfig,
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
}
