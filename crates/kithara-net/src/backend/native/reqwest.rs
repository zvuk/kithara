pub(crate) use ::reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode};
use kithara_platform::time::Duration;

use super::metrics::CountConnectionsLayer;
use crate::{metrics::ConnectionMetrics, types::NetOptions};

pub(crate) type BackendError = ::reqwest::Error;

/// Build the HTTP `Client` (`client-reqwest` backend, native): pure-Rust TLS
/// (`tls-rustls`/`tls-native`), no browser emulation. The stall timeout lives
/// in `resumable_body`, not a client-level wall-clock timer (see the
/// `client-wreq` arm for the `flash` rationale).
pub(crate) fn build_client(
    options: &NetOptions,
    metrics: &ConnectionMetrics,
) -> Result<Client, BackendError> {
    let base = Client::builder()
        .connector_layer(CountConnectionsLayer::new(metrics.clone()))
        .cookie_store(true)
        .pool_max_idle_per_host(options.pool_max_idle_per_host)
        .pool_idle_timeout(Some(Duration::from_secs(5)))
        .danger_accept_invalid_certs(options.is_insecure);
    super::apply_compression(base, options.compression).build()
}
