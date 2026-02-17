#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns))]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

#[doc(hidden)]
pub use crate::timeout::TimeoutNet;
pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult},
    traits::{ByteStream, Net, NetExt},
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};
