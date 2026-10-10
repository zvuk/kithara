use kithara_bufpool::HasPool;

use crate::{ElasticConfig, ElasticEngine, ElasticError, StretchKind, backends};

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
        #[cfg(all(feature = "stretch-signalsmith", not(target_arch = "wasm32")))]
        StretchKind::Signalsmith => backends::SignalsmithElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
        #[cfg(all(feature = "stretch-bungee", not(target_arch = "wasm32")))]
        StretchKind::Bungee => backends::BungeeElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
        #[cfg(feature = "stretch-glide")]
        StretchKind::Glide => backends::VarispeedElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
        #[cfg(feature = "stretch-identity")]
        StretchKind::Identity => backends::IdentityElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
    }
}

/// Prepares the exact-span engine used when pitch follows transport.
/// A selected backend without rate support preserves unity spans.
///
/// # Errors
/// Returns [`ElasticError`] when the configured shape cannot be prepared.
pub fn build_varispeed_engine<S>(
    config: ElasticConfig<S>,
) -> Result<Box<dyn ElasticEngine>, ElasticError>
where
    S: HasPool<f32>,
{
    match config.backend() {
        #[cfg(feature = "stretch-identity")]
        StretchKind::Identity => build_engine(config),
        #[cfg(any(
            feature = "stretch-signalsmith",
            feature = "stretch-bungee",
            feature = "stretch-glide"
        ))]
        _ => backends::VarispeedElastic::prepare(config)
            .map(|engine| Box::new(engine) as Box<dyn ElasticEngine>),
    }
}
