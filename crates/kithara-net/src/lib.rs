#![forbid(unsafe_code)]

#[cfg(all(
    not(target_arch = "wasm32"),
    not(feature = "client-wreq"),
    not(feature = "client-reqwest")
))]
compile_error!(
    "kithara-net: enable one HTTP client backend — `client-reqwest` (default) or `client-wreq`"
);
#[cfg(all(target_arch = "wasm32", not(feature = "client-reqwest")))]
compile_error!(
    "kithara-net: wasm32 requires `client-reqwest` (`client-wreq`/BoringSSL is native-only)"
);

mod backend;
mod client;
mod error;
mod resumable;
mod retry;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "mock"))]
pub mod mock {
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::traits::NetMock;
}

pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult, Retryability},
    timeout::TimeoutNet,
    traits::{ByteStream, Net, NetExt},
    types::{Compression, Headers, ImpersonatePreset, NetOptions, RangeSpec, RetryPolicy},
};
