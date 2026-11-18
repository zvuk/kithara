#![forbid(unsafe_code)]

mod backend;
mod client;
mod error;
mod metrics;
mod observe;
mod range_response;
mod resumable;
mod retry;
#[cfg(test)]
pub(crate) use kithara_test_utils::bufpool as test_pools;
mod traits;
mod transport;
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

pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult, Retryability},
    observe::{NetObserver, Observer},
    traits::{ByteStream, HttpBody, Net},
    transport::HttpTransport,
    types::{
        Compression, CompressionAlgorithm, Headers, ImpersonatePreset, NetOptions, NetOptionsPatch,
        RangeSpec, RetryPolicy, RetryPolicyPatch,
    },
};
