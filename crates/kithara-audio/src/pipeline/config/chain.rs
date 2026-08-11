use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;

use crate::{
    tempo::{TempoSlot, TempoSlotError},
    traits::AudioEffect,
};

/// Build `[Tempo?, ..custom]`. Fixed-ratio sample-rate conversion belongs to
/// the decoder plan.
///
/// # Errors
///
/// Returns [`TempoSlotError`] when the configured slot cannot be built on this
/// target.
pub(crate) fn create_effects(
    initial_spec: PcmSpec,
    tempo: Option<&TempoSlot>,
    pool: &PcmPool,
    custom_effects: Vec<Box<dyn AudioEffect>>,
) -> Result<Vec<Box<dyn AudioEffect>>, TempoSlotError> {
    let mut chain: Vec<Box<dyn AudioEffect>> = Vec::new();

    if let Some(tempo) = tempo {
        append_tempo_slot(tempo, &mut chain, initial_spec, pool)?;
    }
    chain.extend(custom_effects);
    Ok(chain)
}

#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
fn append_tempo_slot(
    tempo: &TempoSlot,
    chain: &mut Vec<Box<dyn AudioEffect>>,
    initial_spec: PcmSpec,
    pool: &PcmPool,
) -> Result<(), TempoSlotError> {
    chain.push(
        crate::tempo::bound::resident_slot(tempo.clone(), initial_spec, pool.clone())
            .map_err(|_| TempoSlotError::BoundEngineMissing)?,
    );
    Ok(())
}

#[cfg(all(
    not(target_arch = "wasm32"),
    not(feature = "stretch-signalsmith"),
    feature = "stretch-bungee"
))]
fn append_tempo_slot(
    tempo: &TempoSlot,
    chain: &mut Vec<Box<dyn AudioEffect>>,
    initial_spec: PcmSpec,
    pool: &PcmPool,
) -> Result<(), TempoSlotError> {
    use crate::tempo::streaming::TimeStretchProcessor;

    chain.push(Box::new(TimeStretchProcessor::new(
        kithara_platform::sync::Arc::clone(tempo.controls()),
        initial_spec,
        pool.clone(),
    )));
    Ok(())
}

/// No stretch backend compiled in: speed DSP is absent and playback is pinned
/// to unity. An unbound deck degrades to that; a bound deck cannot, because
/// unity is not where its beats belong, so binding is refused.
#[cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]
fn append_tempo_slot(
    tempo: &TempoSlot,
    _chain: &mut Vec<Box<dyn AudioEffect>>,
    _initial_spec: PcmSpec,
    _pool: &PcmPool,
) -> Result<(), TempoSlotError> {
    let _ = tempo;
    Ok(())
}
