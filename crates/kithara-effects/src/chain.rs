use kithara_signal::AudioChunk;

use crate::{AudioEffect, EffectPrepareError};

pub(crate) fn check_geometry(chunk: &AudioChunk) -> Result<(), EffectPrepareError> {
    let channels = usize::from(chunk.spec().channels);
    if channels == 0 || !chunk.samples.len().is_multiple_of(channels) {
        return Err(EffectPrepareError::Shape);
    }
    Ok(())
}

pub(crate) fn check_input(
    effect: &dyn AudioEffect,
    chunk: &AudioChunk,
) -> Result<(), EffectPrepareError> {
    check_geometry(chunk)?;
    if !effect.accepts_spec(chunk.spec()) {
        return Err(EffectPrepareError::Shape);
    }
    Ok(())
}

/// Apply the effect chain to the chunk.
/// # Errors
/// Refuses unsupported formats or incomplete frames before processing each stage.
pub fn apply_effects(
    effects: &mut [Box<dyn AudioEffect>],
    mut chunk: AudioChunk,
) -> Result<Option<AudioChunk>, EffectPrepareError> {
    for effect in &mut *effects {
        check_input(&**effect, &chunk)?;
        let Some(output) = effect.process(chunk) else {
            return Ok(None);
        };
        chunk = output;
    }
    check_geometry(&chunk)?;
    Ok(Some(chunk))
}

/// Frames the chain still holds back from the source it was fed.
#[must_use]
pub fn held_source_frames(effects: &[Box<dyn AudioEffect>]) -> u64 {
    effects.iter().fold(0_u64, |total, effect| {
        total.saturating_add(effect.held_source_frames())
    })
}

/// Reset effects chain (e.g. after seek).
pub fn reset_effects(effects: &mut [Box<dyn AudioEffect>]) {
    for effect in &mut *effects {
        effect.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_test_utils::{
        bufpool::{pools, sample_buffer},
        kithara,
    };

    use super::*;
    use crate::EffectDrain;

    struct Tail {
        spec: AudioSpec,
        output: Option<AudioChunk>,
    }

    impl AudioEffect for Tail {
        fn accepts_spec(&self, spec: AudioSpec) -> bool {
            spec == self.spec
        }
        fn process(&mut self, _: AudioChunk) -> Option<AudioChunk> {
            panic!("refused PCM reached a processing stage");
        }
        fn flush(&mut self) -> Option<AudioChunk> {
            self.output.take()
        }
        fn held_source_frames(&self) -> u64 {
            0
        }
        fn reset(&mut self) {}
    }

    fn spec(channels: u16) -> AudioSpec {
        AudioSpec::new(channels, NonZeroU32::new(48_000).unwrap())
    }

    #[kithara::test]
    fn effect_chain_refuses_geometry_and_spec_before_processing() {
        let region = pools();
        for (channels, count) in [(0, 0), (2, 3), (1, 4)] {
            let chunk = AudioChunk::new(
                AudioChunkInfo {
                    spec: spec(channels),
                    ..Default::default()
                },
                sample_buffer(&region, &vec![0.5; count]),
            );
            let mut effects: Vec<Box<dyn AudioEffect>> = vec![Box::new(Tail {
                spec: spec(2),
                output: None,
            })];
            assert!(matches!(
                apply_effects(&mut effects, chunk),
                Err(EffectPrepareError::Shape)
            ));
        }
    }

    #[kithara::test]
    fn effect_drain_refuses_incomplete_flush_and_downstream_format() {
        let region = pools();
        for (channels, count) in [(2, 3), (1, 4)] {
            let format = spec(channels);
            let tail = AudioChunk::new(
                AudioChunkInfo {
                    spec: format,
                    ..Default::default()
                },
                sample_buffer(&region, &vec![0.5; count]),
            );
            let mut effects: Vec<Box<dyn AudioEffect>> = vec![
                Box::new(Tail {
                    spec: format,
                    output: Some(tail),
                }),
                Box::new(Tail {
                    spec: spec(2),
                    output: None,
                }),
            ];
            let mut drain = EffectDrain::new(effects.len(), &region).unwrap();
            assert!(matches!(
                drain.step(&mut effects),
                Err(EffectPrepareError::Shape)
            ));
        }
    }
}
