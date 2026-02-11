#![forbid(unsafe_code)]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "test-utils"))]
pub use unimock;

#[doc(hidden)]
pub use crate::timeout::TimeoutNet;
#[cfg(any(test, feature = "test-utils"))]
pub use crate::traits::NetMock;
pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult},
    traits::{ByteStream, Net, NetExt},
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};
