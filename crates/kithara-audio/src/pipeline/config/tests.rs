use std::num::NonZeroU32;

use kithara_bufpool::{BytePool, PcmPool};
use kithara_decode::{PcmChunk, PcmSpec};
use kithara_test_utils::kithara;

use super::create_effects;
use crate::{effects::timestretch::StretchControls, traits::AudioEffect};

struct PassthroughEffect;

impl AudioEffect for PassthroughEffect {
    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        Some(chunk)
    }
    fn reset(&mut self) {}
}

fn spec() -> PcmSpec {
    PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate"))
}

fn pool() -> PcmPool {
    PcmPool::default().clone()
}

#[kithara::test]
fn create_effects_includes_custom_effects() {
    let pool = pool();
    let effects = create_effects(spec(), None, &pool, vec![Box::new(PassthroughEffect)]);
    assert_eq!(effects.len(), 1);
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use kithara_assets::{AssetStoreBuilder, StorageBackend};
    use kithara_file::{FileConfig, FileSrc};
    use kithara_resampler::NoResamplerBackend;

    use super::*;
    use crate::pipeline::config::AudioConfig;

    #[kithara::test]
    fn audio_config_with_effect_adds_to_chain() {
        let effects: Vec<Box<dyn AudioEffect>> =
            vec![Box::new(PassthroughEffect), Box::new(PassthroughEffect)];
        let config =
            AudioConfig::<kithara_file::File, NoResamplerBackend>::for_stream(FileConfig::new(
                FileSrc::Local(std::env::temp_dir().join("kithara-audio-config.wav")),
                AssetStoreBuilder::default()
                    .backend(StorageBackend::Memory)
                    .build(),
            ))
            .byte_pool(BytePool::default())
            .pcm_pool(PcmPool::default())
            .effects(effects)
            .build();
        assert_eq!(config.effects().len(), 2);
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
    fn create_effects_stretch_without_backends_keeps_chain_empty() {
        let controls = StretchControls::new(1.5);
        let pool = pool();
        let effects = create_effects(spec(), Some(&controls), &pool, Vec::new());
        assert!(effects.is_empty());
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod stretch {
    use kithara_decode::PcmMeta;

    use super::*;

    #[kithara::test]
    fn create_effects_tempo_mode_prepends_stretch_slot() {
        let controls = StretchControls::new(1.0);
        let pool = pool();
        let effects = create_effects(
            spec(),
            Some(&controls),
            &pool,
            vec![Box::new(PassthroughEffect)],
        );
        assert_eq!(effects.len(), 2);
    }

    /// Key-lock off in tempo mode is still handled by the stretch slot.
    #[kithara::test]
    fn create_effects_tempo_vinyl_uses_stretch_slot() {
        let controls = StretchControls::new(1.5);
        controls.set_keylock(false);
        let pool = pool();
        let mut effects = create_effects(spec(), Some(&controls), &pool, Vec::new());
        // Drive one chunk through the stretch slot (index 0).
        let frames = 1024usize;
        let samples = vec![0.0_f32; frames * 2];
        let meta = PcmMeta {
            spec: spec(),
            frames: u32::try_from(frames).unwrap(),
            ..Default::default()
        };
        let chunk = PcmChunk::new(meta, PcmPool::default().attach(samples.clone()));
        let out = effects[0]
            .process(chunk)
            .expect("vinyl stretch emits a chunk");
        assert_eq!(out.spec(), spec());
        assert!(!out.samples.is_empty());
    }
}
