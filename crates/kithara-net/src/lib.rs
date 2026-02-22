#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::ignored_unit_patterns))]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult},
    traits::{ByteStream, Net, NetExt},
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};
