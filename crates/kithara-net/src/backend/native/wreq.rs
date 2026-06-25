pub(crate) use ::wreq::{Client, ClientBuilder, RequestBuilder, Response};
use kithara_platform::time::Duration;

use crate::types::{ImpersonatePreset, NetOptions};

pub(crate) type BackendError = ::wreq::Error;

impl From<ImpersonatePreset> for ::wreq_util::Emulation {
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
pub(crate) fn build_client(options: &NetOptions) -> Result<Client, BackendError> {
    let base = Client::builder()
        .emulation(::wreq_util::Emulation::from(options.impersonate))
        .cookie_store(true)
        .pool_max_idle_per_host(options.pool_max_idle_per_host)
        .pool_idle_timeout(Some(Duration::from_secs(5)))
        .cert_verification(!options.is_insecure);
    super::apply_compression(base, options.compression).build()
}
