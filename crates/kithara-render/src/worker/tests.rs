use kithara_assets::{AssetStore, StorageBackend};
use kithara_audio::{AudioConfig, NoResamplerBackend};
use kithara_file::{File, FileConfig, FileSrc};
use kithara_test_utils::kithara;

use super::TrackConfig;
use crate::test_pools::{TestPools, pools};

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
fn audio_config_keeps_the_native_ring_and_preload_defaults() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let config = TrackConfig::for_audio(audio).build();

    assert_eq!(config.audio_buffer_chunks().get(), 10);
    assert_eq!(config.preload_chunks().get(), 3);
}

#[kithara::test]
fn the_document_names_both_live_keys() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let config = TrackConfig::for_audio(audio)
        .preload_chunks(std::num::NonZeroUsize::new(8).expect("preload"))
        .audio_buffer_chunks(std::num::NonZeroUsize::new(20).expect("capacity"))
        .build();
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(config.audio_buffer_chunks().get(), 20);
}

#[kithara::test]
fn an_absent_key_stays_unset_rather_than_defaulting() {
    let audio =
        AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file_config()).build();
    let config = TrackConfig::for_audio(audio)
        .preload_chunks(std::num::NonZeroUsize::new(8).expect("preload"))
        .maybe_audio_buffer_chunks(None)
        .build();
    assert_eq!(
        config.preload_chunks(),
        std::num::NonZeroUsize::new(8).expect("preload")
    );
    assert_eq!(
        config.audio_buffer_chunks(),
        crate::consts::CAPACITY,
        "an unnamed value leaves the owner's capacity unchanged"
    );
}
