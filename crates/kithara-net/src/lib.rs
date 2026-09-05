#![forbid(unsafe_code)]

mod backend;
#[cfg(not(all(feature = "client-apple", any(target_os = "macos", target_os = "ios"))))]
mod client;
mod error;
mod metrics;
mod observe;
mod range_response;
mod resumable;
mod retry;
#[cfg(test)]
pub(crate) use kithara_bufpool::testing as test_pools;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "mock"))]
pub mod mock {
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::traits::NetMock;
}

pub use crate::{
    backend::HttpClient,
    error::{NetError, NetResult, Retryability},
    observe::{NetObserver, Observer},
    timeout::TimeoutNet,
    traits::{ByteStream, Net, NetExt},
    types::{
        Compression, CompressionAlgorithm, Headers, ImpersonatePreset, NetOptions, NetOptionsPatch,
        RangeSpec, RetryPolicy, RetryPolicyPatch,
    },
};
