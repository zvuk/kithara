#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
compile_error!(
    "kithara-stretch requires at least one backend feature: \
     enable stretch-signalsmith (default) or stretch-bungee. \
     A build with no stretch backend should not depend on this crate."
);

mod backend;
pub use backend::{StretchBackend, StretchBackendError};

mod config;
pub use config::StretchOptions;

mod kind;
pub use kind::StretchKind;

mod factory;
pub use factory::build_backend;

mod backends;
#[cfg(feature = "stretch-signalsmith")]
pub use backends::SignalsmithBackend;

mod elastic;
pub use elastic::{
    ElasticCapabilities, ElasticConfig, ElasticCursor, ElasticError, ElasticLatency,
    ElasticRateEnvelope, ElasticRequest, ElasticSpan, ElasticSpanPlan, ElasticSpanRequest,
};
