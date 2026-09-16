use kithara_bufpool::HasPool;

#[cfg(feature = "stretch-bungee")]
use crate::backends::BungeeElastic;
#[cfg(feature = "stretch-signalsmith")]
use crate::backends::SignalsmithElastic;
use crate::{ElasticConfig, ElasticEngine, ElasticError, StretchKind, backends::VarispeedElastic};

/// Prepares the selected exact-span engine.
///
/// # Errors
/// Returns [`ElasticError`] when the config cannot prepare the selected
/// engine.
pub fn build_engine<S>(config: ElasticConfig<S>) -> Result<Box<dyn ElasticEngine>, ElasticError>
where
    S: HasPool<f32>,
{
    match config.backend() {
        #[cfg(feature = "stretch-signalsmith")]
        StretchKind::Signalsmith => SignalsmithElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
        #[cfg(feature = "stretch-bungee")]
        StretchKind::Bungee => {
            BungeeElastic::prepare(config).map(|engine| Box::new(engine) as Box<dyn ElasticEngine>)
        }
    }
}

/// Prepares the exact-span varispeed engine used when pitch follows transport.
///
/// # Errors
/// Returns [`ElasticError`] when the configured shape cannot be prepared.
pub fn build_varispeed_engine<S>(
    config: ElasticConfig<S>,
) -> Result<Box<dyn ElasticEngine>, ElasticError>
where
    S: HasPool<f32>,
{
    VarispeedElastic::prepare(config).map(|engine| Box::new(engine) as Box<dyn ElasticEngine>)
}
