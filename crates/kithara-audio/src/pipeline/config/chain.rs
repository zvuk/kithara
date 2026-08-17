use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;
use kithara_platform::sync::Arc;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use crate::effects::timestretch::TimeStretchProcessor;
use crate::{
    effects::timestretch::StretchControls,
    traits::{AudioEffect, TempoStage},
};

/// The single duration-changing owner followed by frame-preserving effects.
pub(crate) struct PresentationChain {
    pub(crate) effects: Vec<Box<dyn AudioEffect>>,
    pub(crate) tempo: Option<Box<dyn TempoStage>>,
}

impl PresentationChain {
    pub(crate) fn identity(effects: Vec<Box<dyn AudioEffect>>) -> Self {
        Self {
            effects,
            tempo: None,
        }
    }
}

/// Build `{ tempo: Stretch?, effects: custom }`. Fixed-ratio sample-rate
/// conversion belongs to the decoder plan.
pub(crate) fn create_presentation_chain(
    initial_spec: PcmSpec,
    stretch: Option<&Arc<StretchControls>>,
    pool: &PcmPool,
    effects: Vec<Box<dyn AudioEffect>>,
) -> PresentationChain {
    match create_tempo_stage(stretch, initial_spec, pool) {
        Some(tempo) => PresentationChain {
            effects,
            tempo: Some(tempo),
        },
        None => PresentationChain::identity(effects),
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
fn create_tempo_stage(
    controls: Option<&Arc<StretchControls>>,
    initial_spec: PcmSpec,
    pool: &PcmPool,
) -> Option<Box<dyn TempoStage>> {
    controls.map(|controls| {
        let tempo: Box<dyn TempoStage> = Box::new(TimeStretchProcessor::new(
            Arc::clone(controls),
            initial_spec,
            pool.clone(),
        ));
        tempo
    })
}

#[cfg(not(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
)))]
fn create_tempo_stage(
    _controls: Option<&Arc<StretchControls>>,
    _initial_spec: PcmSpec,
    _pool: &PcmPool,
) -> Option<Box<dyn TempoStage>> {
    None
}
