#![forbid(unsafe_code)]

mod client;
mod error;
mod retry;
mod timeout;
mod traits;
mod types;

pub use crate::client::HttpClient;
pub use crate::error::{NetError, NetResult};
pub use crate::timeout::TimeoutNet;
pub use crate::traits::{ByteStream, Net, NetExt};
pub use crate::types::{Headers, NetOptions, RangeSpec, RetryPolicy};
