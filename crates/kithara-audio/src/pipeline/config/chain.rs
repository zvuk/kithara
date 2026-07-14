use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;
use kithara_platform::sync::Arc;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use crate::effects::timestretch::TimeStretchProcessor;
use crate::{effects::timestretch::StretchControls, traits::AudioEffect};

/// Build `[Stretch?, ..custom]`. Fixed-ratio sample-rate conversion belongs to
/// the decoder plan.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    stretch: Option<&Arc<StretchControls>>,
    pool: &PcmPool,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Vec<Box<dyn AudioEffect>> {
    let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();

    append_stretch_slot(stretch, &mut chain, initial_spec, pool);
    chain.extend(custom_effects);
    chain
}

/// Tempo mode with a compiled-in backend: prepend the stretch slot.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
fn append_stretch_slot(
    controls: Option<&Arc<StretchControls>>,
    chain: &mut Vec<Box<dyn AudioEffect>>,
    initial_spec: PcmSpec,
    pool: &PcmPool,
) {
    let Some(controls) = controls else {
        return;
    };
    chain.push(Box::new(TimeStretchProcessor::new(
        Arc::clone(controls),
        initial_spec,
        pool.clone(),
    )));
}

/// No stretch backend compiled in: speed DSP is absent and playback is pinned
/// to unity.
#[cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]
fn append_stretch_slot(
    _controls: Option<&Arc<StretchControls>>,
    _chain: &mut Vec<Box<dyn AudioEffect>>,
    _initial_spec: PcmSpec,
    _pool: &PcmPool,
) {
}
