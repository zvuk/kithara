mod capabilities;
pub use capabilities::ElasticCapabilities;

mod config;
pub use config::{ElasticConfig, ElasticSpanConfig};

mod error;
pub use error::ElasticError;

mod latency;
pub use latency::ElasticLatency;

mod rate;
pub use rate::ElasticRateEnvelope;

mod request;
pub use request::ElasticRequest;

mod span;
pub use span::{ElasticCursor, ElasticSpan, ElasticSpanPlan, ElasticSpanRequest};

#[cfg(all(test, feature = "stretch-signalsmith"))]
mod tests;

#[cfg(all(test, feature = "stretch-signalsmith"))]
mod span_tests;
