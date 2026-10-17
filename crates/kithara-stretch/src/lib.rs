#[cfg(not(any(
    all(feature = "stretch-signalsmith", not(target_arch = "wasm32")),
    all(feature = "stretch-bungee", not(target_arch = "wasm32")),
    feature = "stretch-glide",
    feature = "stretch-identity"
)))]
compile_error!(
    "kithara-stretch requires at least one backend feature: \
     enable stretch-signalsmith (default), stretch-bungee, stretch-glide, or stretch-identity. \
     A build with no stretch backend should not depend on this crate."
);

mod kind;
pub use kind::{BackendCapabilities, StretchKind};

mod factory;
pub use factory::{build_engine, build_varispeed_engine};

mod backends;

mod elastic;
pub use elastic::{
    BungeeConfig, BungeeConfigPatch, ElasticBackendConfig, ElasticBackendConfigPatch,
    ElasticBackendConfigPatchError, ElasticCapabilities, ElasticConfig, ElasticCursor,
    ElasticDrain, ElasticEngine, ElasticError, ElasticLatency, ElasticLatencyTag,
    ElasticRateEnvelope, ElasticRequest, ElasticSpan, ElasticSpanConfig, ElasticSpanPlan,
    ElasticSpanRequest, SignalsmithConfig, SignalsmithConfigPatch, SignalsmithConfigPatchError,
};
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod consts;
