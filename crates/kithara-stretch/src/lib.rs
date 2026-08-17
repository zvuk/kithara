#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
compile_error!(
    "kithara-stretch requires at least one backend feature: \
     enable stretch-signalsmith (default) or stretch-bungee. \
     A build with no stretch backend should not depend on this crate."
);

mod backend;
pub use backend::{DrainDisposition, StretchBackend, StretchBackendError};

mod config;
pub use config::StretchOptions;

mod kind;
pub use kind::StretchKind;

mod factory;
pub use factory::build_backend;

mod backends;
#[cfg(feature = "stretch-bungee")]
pub use backends::BungeeElastic;
#[cfg(feature = "stretch-signalsmith")]
pub use backends::SignalsmithElastic;

mod elastic;
pub use elastic::{
    ElasticCapabilities, ElasticConfig, ElasticCursor, ElasticEngine, ElasticError, ElasticLatency,
    ElasticPriming, ElasticRateEnvelope, ElasticRequest, ElasticSpan, ElasticSpanConfig,
    ElasticSpanPlan, ElasticSpanRequest,
};
