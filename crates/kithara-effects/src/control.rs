use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_signal::AudioSpec;

use crate::{
    AudioEffect,
    eq::{EqBandConfig, EqBandConfigChange, EqConfig, EqEffect},
    filter::{self, FilterConfig, FilterEffect, FilterSettings, FilterSettingsChange},
};

/// Construction intent prepared once in the track's audio format.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum EffectConfig {
    Equalizer(Vec<EqBandConfig>),
    Filter(FilterSettings),
}

/// Construction failure before an effect enters audio production.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EffectPrepareError {
    #[error("the prepared effect does not admit this PCM format or frame geometry")]
    Shape,
    #[error(transparent)]
    Pool(#[from] PoolError),
    #[error(transparent)]
    Filter(#[from] kithara_dsp::filter::FilterError),
}

impl From<filter::PrepareError> for EffectPrepareError {
    fn from(error: filter::PrepareError) -> Self {
        match error {
            filter::PrepareError::Filter(error) => Self::Filter(error),
            filter::PrepareError::Pool(error) => Self::Pool(error),
        }
    }
}

/// One chain entry, transferred to its producer before playback.
#[non_exhaustive]
pub enum EffectSetup {
    Config(EffectConfig),
    Prepared(Box<dyn AudioEffect>),
}

impl EffectSetup {
    /// Prepare an entry outside checked audio production.
    /// # Errors
    /// Returns the effect's format or pool admission failure.
    pub fn prepare<S: HasPool<f32> + Send + Sync + 'static>(
        self,
        spec: AudioSpec,
        pools: &PoolRegion<S>,
    ) -> Result<Box<dyn AudioEffect>, EffectPrepareError> {
        if spec.channels == 0 {
            return Err(EffectPrepareError::Shape);
        }
        match self {
            Self::Prepared(mut effect) => {
                effect.service_deferred(spec)?;
                if !effect.accepts_spec(spec) {
                    return Err(EffectPrepareError::Shape);
                }
                Ok(effect)
            }
            Self::Config(EffectConfig::Filter(settings)) => Ok(Box::new(FilterEffect::new(
                spec,
                FilterConfig::from((pools.clone(), settings)),
            )?)),
            Self::Config(EffectConfig::Equalizer(bands)) => Ok(Box::new(EqEffect::new(
                &EqConfig::builder(pools.clone()).build(),
                bands,
                spec.sample_rate.get(),
                spec.channels,
            )?)),
        }
    }
}

/// A parameter in the fixed effect chain of one loaded track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum EffectTarget {
    EqBand { effect: usize, band: usize },
    Filter { effect: usize },
}

impl EffectTarget {
    /// Index in the track's prepared effect chain.
    #[must_use]
    pub const fn effect(self) -> usize {
        match self {
            Self::EqBand { effect, .. } | Self::Filter { effect } => effect,
        }
    }
}

/// One checked configuration field applied before the target output frame.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum EffectChange {
    EqBand(EqBandConfigChange),
    Filter(FilterSettingsChange),
}

impl From<FilterSettingsChange> for EffectChange {
    fn from(change: FilterSettingsChange) -> Self {
        Self::Filter(change)
    }
}

impl From<EqBandConfigChange> for EffectChange {
    fn from(change: EqBandConfigChange) -> Self {
        Self::EqBand(change)
    }
}

/// A change cannot apply to the addressed prepared effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the change is not admitted by the prepared effect")]
#[non_exhaustive]
pub struct EffectRefusal;
