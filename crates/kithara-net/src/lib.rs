#![forbid(unsafe_code)]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "test-utils"))]
pub use unimock;

#[cfg(any(test, feature = "test-utils"))]
pub use crate::traits::NetMock;
pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult},
    timeout::TimeoutNet,
    traits::{ByteStream, Net, NetExt},
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};
