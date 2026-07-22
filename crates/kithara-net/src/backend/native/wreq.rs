pub(crate) use ::wreq::{Client, ClientBuilder, RequestBuilder, Response, StatusCode};
use kithara_platform::time::Duration;

use super::metrics::CountConnectionsLayer;
use crate::{
    metrics::ConnectionMetrics,
    types::{ImpersonatePreset, NetOptions},
};

pub(crate) type BackendError = ::wreq::Error;

impl From<ImpersonatePreset> for ::wreq_util::Profile {
    fn from(p: ImpersonatePreset) -> Self {
        match p {
            ImpersonatePreset::Safari => Self::Safari18,
            ImpersonatePreset::Chrome => Self::Chrome137,
        }
    }
}

/// Build the HTTP `Client` (`client-wreq` backend): `BoringSSL` plus browser
/// TLS/HTTP2 emulation so anti-bot WAFs that fingerprint the `ClientHello`
/// (JA3) see a real browser. Applies pool / cert / compression knobs.
///
/// No client-level `.read_timeout`: the idle/stall timeout is owned by the
/// resilient body (`resumable_body`), whose `sleep(inactivity_timeout)` routes
/// through `kithara_platform::time` and so collapses under `flash`. A
/// wall-clock client timer would double-own the stall and break simulation
/// determinism.
pub(crate) fn build_client(
    options: &NetOptions,
    metrics: &ConnectionMetrics,
) -> Result<Client, BackendError> {
    let base = Client::builder()
        .connector_layer(CountConnectionsLayer::new(metrics.clone()))
        .emulation(::wreq_util::Profile::from(options.impersonate))
        .cookie_store(true)
        .pool_max_idle_per_host(options.pool_max_idle_per_host)
        .pool_idle_timeout(Some(Duration::from_secs(5)))
        .tls_cert_verification(!options.is_insecure);
    super::apply_compression(base, options.compression).build()
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test]
    #[case::safari(ImpersonatePreset::Safari, ::wreq_util::Profile::Safari18)]
    #[case::chrome(ImpersonatePreset::Chrome, ::wreq_util::Profile::Chrome137)]
    fn impersonation_profile(
        #[case] preset: ImpersonatePreset,
        #[case] expected: ::wreq_util::Profile,
    ) {
        assert_eq!(::wreq_util::Profile::from(preset), expected);
    }
}
