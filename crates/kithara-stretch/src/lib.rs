#[cfg(not(any(
    feature = "stretch-signalsmith",
    feature = "stretch-bungee",
    feature = "stretch-glide"
)))]
compile_error!(
    "kithara-stretch requires at least one backend feature: \
     enable stretch-signalsmith (default), stretch-bungee, or stretch-glide. \
     A build with no stretch backend should not depend on this crate."
);

mod kind;
pub use kind::{BackendCapabilities, StretchKind};

mod factory;
pub use factory::{build_engine, build_varispeed_engine};

mod backends;

mod elastic;
pub use elastic::{
    BungeeConfig, BungeeConfigPatch, BungeeConfigValues, ElasticBackendConfig,
    ElasticBackendConfigPatch, ElasticBackendConfigValues, ElasticCapabilities, ElasticConfig,
    ElasticConfigValues, ElasticCursor, ElasticDrain, ElasticEngine, ElasticError, ElasticLatency,
    ElasticRateEnvelope, ElasticRequest, ElasticShapeValues, ElasticSpan, ElasticSpanConfig,
    ElasticSpanConfigValues, ElasticSpanPlan, ElasticSpanRequest, SignalsmithConfig,
    SignalsmithConfigPatch, SignalsmithConfigValues,
};
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
