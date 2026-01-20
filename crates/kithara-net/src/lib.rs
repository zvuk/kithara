#![forbid(unsafe_code)]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

pub use crate::{
    client::HttpClient,
    error::{NetError, NetResult},
    timeout::TimeoutNet,
    traits::{ByteStream, Net, NetExt},
    types::{Headers, NetOptions, RangeSpec, RetryPolicy},
};

#[cfg(any(test, feature = "test-utils"))]
pub use crate::traits::MockNet;
