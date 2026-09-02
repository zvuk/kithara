#[cfg(not(any(feature = "stretch-signalsmith", feature = "stretch-bungee")))]
compile_error!(
    "kithara-stretch requires at least one backend feature: \
     enable stretch-signalsmith (default) or stretch-bungee. \
     A build with no stretch backend should not depend on this crate."
);

mod kind;
pub use kind::StretchKind;

mod factory;
pub use factory::build_engine;

mod backends;

mod elastic;
pub use elastic::{
    BungeeConfig, ElasticBackendConfig, ElasticCapabilities, ElasticConfig, ElasticCursor,
    ElasticDrain, ElasticEngine, ElasticError, ElasticLatency, ElasticRateEnvelope, ElasticRequest,
    ElasticSpan, ElasticSpanConfig, ElasticSpanPlan, ElasticSpanRequest, SignalsmithConfig,
};
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
