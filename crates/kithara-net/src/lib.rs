#![forbid(unsafe_code)]

mod backend;
#[cfg(reqwest_backend)]
mod client;
mod error;
#[cfg(not(feature = "client-host"))]
mod metrics;
mod observe;
mod range_response;
mod resumable;
mod retry;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "mock"))]
pub mod mock {
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::traits::NetMock;
}

/// The protocol between the `client-host` backend and the transport a host
/// application installs once per process.
#[cfg(feature = "client-host")]
pub mod host {
    pub use crate::backend::host::{
        AlreadyInstalled, HostBuffer, HostCall, HostEvents, HostFailure, HostMethod, HostRequest,
        HostRequestBody, HostTransport, install,
    };
}

use humantime_serde as _;

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
