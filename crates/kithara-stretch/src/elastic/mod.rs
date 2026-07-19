mod backend;
pub use backend::ElasticBackend;

mod capabilities;
pub use capabilities::ElasticCapabilities;

mod config;
pub use config::ElasticConfig;

mod error;
pub use error::ElasticError;

mod latency;
pub use latency::ElasticLatency;

mod rate;
pub use rate::ElasticRateEnvelope;

mod request;
pub use request::ElasticRequest;

#[cfg(feature = "stretch-signalsmith")]
mod signalsmith;
#[cfg(feature = "stretch-signalsmith")]
pub use signalsmith::SignalsmithElastic;

#[cfg(all(test, feature = "stretch-signalsmith"))]
mod tests;
