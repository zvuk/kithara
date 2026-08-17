use std::num::NonZeroU32;

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{DecodeResult, PcmSpec};
use kithara_test_utils::kithara;

use super::create_presentation_chain;
use crate::{
    effects::timestretch::StretchControls,
    traits::{AudioBlockMut, AudioEffect},
};

struct PassthroughEffect;

impl AudioEffect for PassthroughEffect {
    fn process(&mut self, _block: AudioBlockMut<'_>) -> DecodeResult<()> {
        Ok(())
    }

    fn reset(&mut self) {}
}

fn spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
}

fn pool() -> PcmPool {
    PcmPool::default()
}

#[kithara::test]
fn create_presentation_chain_includes_custom_effects() {
    let pool = pool();
    let chain = create_presentation_chain(spec(), None, &pool, vec![Box::new(PassthroughEffect)]);
    assert_eq!(chain.effects.len(), 1);
    assert!(chain.tempo.is_none());
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use kithara_assets::{AssetStore, StorageBackend};
    use kithara_file::{FileConfig, FileSrc};
    use kithara_resampler::NoResamplerBackend;

    use super::*;
    use crate::pipeline::config::{AudioConfig, ConsumerWakeMode};

    fn file_config() -> FileConfig {
        FileConfig::for_src(FileSrc::Local(
            std::env::temp_dir().join("kithara-audio-config.wav"),
        ))
        .store(
            AssetStore::builder()
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build()
    }

    #[kithara::test]
    fn audio_config_with_effect_adds_to_chain() {
        let effects: Vec<Box<dyn AudioEffect>> =
            vec![Box::new(PassthroughEffect), Box::new(PassthroughEffect)];
        let config =
            AudioConfig::<kithara_file::File, NoResamplerBackend>::for_stream(file_config())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .effects(effects)
                .build();
        assert_eq!(config.effects().len(), 2);
    }

    #[kithara::test]
    fn audio_config_defaults_to_realtime_deferred_consumer_wakes() {
        let config =
            AudioConfig::<kithara_file::File, NoResamplerBackend>::for_stream(file_config())
                .byte_pool(BytePool::default())
                .pcm_pool(PcmPool::default())
                .build();

        assert_eq!(
            config.consumer_wake_mode(),
            ConsumerWakeMode::RealtimeDeferred
        );
    }
}

#[cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]
mod no_stretch {
    use super::*;

    /// Without a compiled-in stretch backend, `stretch` does not add a speed
    /// slot: playback remains at unity.
    #[kithara::test]
    fn create_presentation_chain_stretch_without_backends_keeps_chain_empty() {
        let controls = StretchControls::new(1.5);
        let pool = pool();
        let chain = create_presentation_chain(spec(), Some(&controls), &pool, Vec::new());
        assert!(chain.effects.is_empty());
        assert!(chain.tempo.is_none());
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod stretch {
    use super::*;

    #[kithara::test]
    fn create_presentation_chain_tempo_mode_owns_stretch_stage() {
        let controls = StretchControls::new(1.0);
        let pool = pool();
        let chain = create_presentation_chain(
            spec(),
            Some(&controls),
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        assert!(chain.tempo.is_some());
        assert_eq!(chain.effects.len(), 1);
    }

    /// Key-lock off in tempo mode is still handled by the stretch slot.
    #[kithara::test]
    fn create_presentation_chain_tempo_vinyl_builds_tempo_stage() {
        let controls = StretchControls::new(1.5);
        controls.set_keylock(false);
        let pool = pool();
        let chain = create_presentation_chain(spec(), Some(&controls), &pool, Vec::new());
        let tempo = chain.tempo.expect("tempo mode owns one duration stage");
        assert_eq!(tempo.output_spec(), spec());
        assert!(chain.effects.is_empty());
    }
}
